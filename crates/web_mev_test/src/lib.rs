//! Test helpers for web apps. Use as a dev-dependency only.

use std::str::FromStr;

use sqlx::{
    SqlitePool,
    migrate::Migrator,
    sqlite::{SqliteConnectOptions, SqlitePoolOptions},
};
pub use web_mev_core::FixedClock;
use web_mev_sqlite::{ConnectError, DbTuning, connect_with};

/// An in-memory database with migrations applied. It has one connection,
/// because each in-memory connection is its own database, so holding a
/// transaction while querying through the pool deadlocks. Use [`TestDb`] when a
/// test needs concurrency.
///
/// # Errors
///
/// Returns [`ConnectError`] when the database can't be opened or migrated.
pub async fn memory_pool(migrator: &Migrator) -> Result<SqlitePool, ConnectError> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(options)
        .await?;
    migrator.run(&pool).await?;
    Ok(pool)
}

/// A real on-disk WAL database in a temporary directory, which also exercises
/// locking and concurrency. It is deleted on drop.
#[derive(Debug)]
pub struct TestDb {
    /// The pool, set up as in production.
    pub pool: SqlitePool,
    /// The database URL.
    pub url: String,
    dir: tempfile::TempDir,
}

impl TestDb {
    /// Creates the database and applies migrations.
    ///
    /// # Errors
    ///
    /// Returns [`ConnectError`] when the database can't be opened or migrated.
    pub async fn new(migrator: &Migrator) -> Result<Self, ConnectError> {
        let dir = tempfile::tempdir().map_err(ConnectError::Directory)?;
        let url = format!("sqlite://{}", dir.path().join("test.db").display());
        let pool = connect_with(&url, migrator, &DbTuning::default()).await?;
        Ok(Self { pool, url, dir })
    }

    /// The temporary directory, for data files beside the database.
    #[must_use]
    pub fn dir(&self) -> &std::path::Path {
        self.dir.path()
    }
}
