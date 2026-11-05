//! SQLite setup and helpers on sqlx, tuned for one server process.
//!
//! `sqlx::migrate!` resolves against the crate that calls it, so the app passes
//! its own [`Migrator`]:
//!
//! ```ignore
//! static MIGRATOR: Migrator = sqlx::migrate!();
//! let pool = web_mev_sqlite::connect_with(&url, &MIGRATOR, &DbTuning::default()).await?;
//! ```

use std::{
    path::{Path, PathBuf},
    str::FromStr,
    time::Duration,
};

use sqlx::{
    Sqlite, SqlitePool, Transaction,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
};

/// Why the database couldn't be opened.
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The directory for the database file couldn't be created.
    #[error("could not create the database directory: {0}")]
    Directory(#[source] std::io::Error),
    /// The database couldn't be opened.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
    /// A migration failed.
    #[error(transparent)]
    Migration(#[from] sqlx::migrate::MigrateError),
}

/// Pool and memory settings. Apps can expose these as configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DbTuning {
    /// Pooled connections.
    pub max_connections: u32,
    /// Page cache of each connection, in KiB.
    pub cache_kib: u32,
    /// Largest memory map of the database file in bytes. 0 turns it off.
    pub mmap_bytes: u64,
    /// How long a connection waits for another writer.
    pub busy_timeout: Duration,
    /// Overwrite deleted rows with zeros, for apps that promise erasure. This
    /// costs extra writes.
    pub secure_delete: bool,
}

impl Default for DbTuning {
    fn default() -> Self {
        Self {
            max_connections: 10,
            cache_kib: 2048,
            mmap_bytes: 1024 * 1024 * 1024,
            busy_timeout: Duration::from_secs(5),
            secure_delete: false,
        }
    }
}

/// The file path of a `sqlite://` URL, unless it is in memory.
fn file_path(url: &str) -> Option<&Path> {
    url.strip_prefix("sqlite://")
        .and_then(|value| value.split('?').next())
        .filter(|path| !path.is_empty() && *path != ":memory:")
        .map(Path::new)
}

/// Directory holding the database file of `url`, where data files beside the
/// database go. It's the working directory for relative or in-memory URLs.
#[must_use]
pub fn data_dir(url: &str) -> PathBuf {
    file_path(url)
        .and_then(Path::parent)
        .filter(|parent| !parent.as_os_str().is_empty())
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Opens the pool and applies `migrator`: WAL, full sync, foreign keys, busy
/// timeout. The file and its directory are created if missing.
///
/// # Errors
///
/// Returns [`ConnectError`] when the directory can't be created, or the database
/// can't be opened or migrated.
pub async fn connect_with(
    url: &str,
    migrator: &Migrator,
    tuning: &DbTuning,
) -> Result<SqlitePool, ConnectError> {
    if let Some(parent) = file_path(url).and_then(Path::parent)
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(ConnectError::Directory)?;
    }
    let options = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Full)
        .foreign_keys(true)
        .busy_timeout(tuning.busy_timeout)
        // Sorts and window functions stay in memory. Reads go through a memory
        // map of the file instead of a `pread` per page miss.
        .pragma("temp_store", "memory")
        .pragma("cache_size", format!("-{}", tuning.cache_kib))
        .pragma("mmap_size", tuning.mmap_bytes.to_string())
        .pragma(
            "secure_delete",
            if tuning.secure_delete { "ON" } else { "OFF" },
        );
    // Connections opened for a burst close after a minute idle, returning their
    // page cache and prepared statements. One stays open.
    let pool = SqlitePoolOptions::new()
        .max_connections(tuning.max_connections)
        .min_connections(1)
        .idle_timeout(Duration::from_secs(60))
        .connect_with(options)
        .await?;
    migrator.run(&pool).await?;
    // Refresh planner statistics where they are missing or stale. This is cheap:
    // a full ANALYZE only runs on tables that need it.
    sqlx::query("PRAGMA optimize = 0x10002")
        .execute(&pool)
        .await?;
    Ok(pool)
}

/// Begins a write transaction that takes the write lock up front.
///
/// A deferred transaction that reads first and writes later fails at once with
/// `SQLITE_BUSY` when another connection committed in between, because WAL
/// snapshots can't be upgraded, and the busy timeout doesn't apply to that case.
/// `BEGIN IMMEDIATE` waits for the lock instead.
///
/// # Errors
///
/// Returns the database error when the lock can't be taken in time. Check it
/// with [`is_busy`] to answer "try again" rather than failing the request.
pub async fn write_tx(db: &SqlitePool) -> Result<Transaction<'static, Sqlite>, sqlx::Error> {
    db.begin_with("BEGIN IMMEDIATE").await
}

/// Whether `error` means the database stayed locked past the busy timeout. It
/// covers `SQLITE_BUSY` and its extended codes.
#[must_use]
pub fn is_busy(error: &sqlx::Error) -> bool {
    matches!(error, sqlx::Error::Database(db) if db
        .code()
        .and_then(|code| code.parse::<i32>().ok())
        .is_some_and(|code| code & 0xff == 5))
}

/// A list of values as one JSON array bind, for `IN (SELECT value FROM
/// json_each(?))`. Unlike `IN (?, ?, ...)`, the SQL text is the same for every
/// list length, so the per-connection statement cache keeps hitting.
#[must_use]
pub fn json_list<T: serde::Serialize>(values: &[T]) -> String {
    serde_json::to_string(values).unwrap_or_else(|_| "[]".to_owned())
}

/// Checks that the database answers queries, for a readiness endpoint.
///
/// # Errors
///
/// Returns the database error when it doesn't.
pub async fn ready(db: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(db)
        .await?;
    Ok(())
}

/// Checks that the database can take the write lock, which also fails on a
/// read-only file. This can wait for the busy timeout behind a long write.
///
/// # Errors
///
/// Returns the database error when the lock can't be taken.
pub async fn ready_for_writes(db: &SqlitePool) -> Result<(), sqlx::Error> {
    write_tx(db).await?.rollback().await
}

/// Loads a generated secret, storing `fresh` on first use. Several instances
/// starting at once all end up with the same value.
///
/// The app owns the table, so it passes the SQL. `insert` takes `fresh` as its
/// one bind and must ignore a conflict, and `select` returns the stored value:
///
/// ```ignore
/// get_or_create_secret(
///     &pool,
///     "INSERT OR IGNORE INTO settings (key, value) VALUES ('signing_key', ?)",
///     "SELECT value FROM settings WHERE key = 'signing_key'",
///     &fresh,
/// )
/// ```
///
/// # Errors
///
/// Returns the database error when either statement fails.
pub async fn get_or_create_secret(
    db: &SqlitePool,
    insert: &'static str,
    select: &'static str,
    fresh: &str,
) -> Result<String, sqlx::Error> {
    sqlx::query(insert).bind(fresh).execute(db).await?;
    sqlx::query_scalar(select).fetch_one(db).await
}
