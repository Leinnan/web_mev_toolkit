//! Behaviour against real database files.

use std::{path::PathBuf, time::Duration};

use sqlx::migrate::Migrator;
use web_mev_sqlite::{
    ConnectError, DbTuning, connect_with, data_dir, get_or_create_secret, is_busy, json_list,
    ready, ready_for_writes, write_tx,
};

static MIGRATOR: Migrator = sqlx::migrate!("tests/migrations");

fn url(dir: &tempfile::TempDir, name: &str) -> String {
    format!("sqlite://{}", dir.path().join(name).display())
}

#[tokio::test]
async fn connect_creates_directories_and_migrates() {
    let dir = tempfile::tempdir().unwrap();
    let pool = connect_with(&url(&dir, "a/b/app.db"), &MIGRATOR, &DbTuning::default())
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('k', 'v')")
        .execute(&pool)
        .await
        .unwrap();
    assert!(dir.path().join("a/b/app.db").exists());
}

#[tokio::test]
async fn pragmas_are_applied_on_every_connection() {
    let dir = tempfile::tempdir().unwrap();
    let tuning = DbTuning {
        cache_kib: 4096,
        secure_delete: true,
        ..DbTuning::default()
    };
    let pool = connect_with(&url(&dir, "app.db"), &MIGRATOR, &tuning)
        .await
        .unwrap();
    let mode: String = sqlx::query_scalar("PRAGMA journal_mode")
        .fetch_one(&pool)
        .await
        .unwrap();
    let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
        .fetch_one(&pool)
        .await
        .unwrap();
    let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
        .fetch_one(&pool)
        .await
        .unwrap();
    let cache: i64 = sqlx::query_scalar("PRAGMA cache_size")
        .fetch_one(&pool)
        .await
        .unwrap();
    let secure: i64 = sqlx::query_scalar("PRAGMA secure_delete")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(mode, "wal");
    assert_eq!(synchronous, 2);
    assert_eq!(foreign_keys, 1);
    assert_eq!(cache, -4096);
    assert_eq!(secure, 1);
}

#[tokio::test]
async fn unusable_directory_is_reported_as_such() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("file"), b"").unwrap();
    let error = connect_with(&url(&dir, "file/app.db"), &MIGRATOR, &DbTuning::default())
        .await
        .unwrap_err();
    assert!(matches!(error, ConnectError::Directory(_)), "{error}");
}

#[test]
fn data_dir_finds_the_parent() {
    assert_eq!(
        data_dir("sqlite:///data/app.db?mode=rwc"),
        PathBuf::from("/data")
    );
    assert_eq!(data_dir("sqlite://app.db"), PathBuf::from("."));
    assert_eq!(data_dir("sqlite://:memory:"), PathBuf::from("."));
    assert_eq!(data_dir("sqlite::memory:"), PathBuf::from("."));
}

#[tokio::test]
async fn write_tx_serializes_read_then_write() {
    let dir = tempfile::tempdir().unwrap();
    let pool = connect_with(&url(&dir, "app.db"), &MIGRATOR, &DbTuning::default())
        .await
        .unwrap();
    sqlx::query("INSERT INTO settings (key, value) VALUES ('n', '0')")
        .execute(&pool)
        .await
        .unwrap();
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let pool = pool.clone();
        tasks.push(tokio::spawn(async move {
            let mut tx = write_tx(&pool).await.unwrap();
            let value: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'n'")
                .fetch_one(&mut *tx)
                .await
                .unwrap();
            let next = value.parse::<i64>().unwrap() + 1;
            sqlx::query("UPDATE settings SET value = ? WHERE key = 'n'")
                .bind(next.to_string())
                .execute(&mut *tx)
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }));
    }
    for task in tasks {
        task.await.unwrap();
    }
    let value: String = sqlx::query_scalar("SELECT value FROM settings WHERE key = 'n'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(value, "8");
}

#[tokio::test]
async fn busy_is_detected_after_the_timeout() {
    let dir = tempfile::tempdir().unwrap();
    let tuning = DbTuning {
        busy_timeout: Duration::from_millis(50),
        ..DbTuning::default()
    };
    let pool = connect_with(&url(&dir, "app.db"), &MIGRATOR, &tuning)
        .await
        .unwrap();
    let held = write_tx(&pool).await.unwrap();
    let error = write_tx(&pool).await.unwrap_err();
    assert!(is_busy(&error), "{error}");
    assert!(ready(&pool).await.is_ok(), "reads continue behind a writer");
    assert!(ready_for_writes(&pool).await.is_err());
    held.rollback().await.unwrap();
    assert!(ready_for_writes(&pool).await.is_ok());
    assert!(!is_busy(&sqlx::Error::RowNotFound));
}

#[tokio::test]
async fn json_list_binds_as_one_value() {
    let db = web_mev_test::memory_pool(&MIGRATOR).await.unwrap();
    let count: i64 = sqlx::query_scalar("SELECT count(*) FROM json_each(?) WHERE value IN (1, 3)")
        .bind(json_list(&[1, 2, 3]))
        .fetch_one(&db)
        .await
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn racing_secrets_converge() {
    let db = web_mev_test::TestDb::new(&MIGRATOR).await.unwrap();
    let mut tasks = Vec::new();
    for index in 0..8 {
        let pool = db.pool.clone();
        tasks.push(tokio::spawn(async move {
            get_or_create_secret(
                &pool,
                "INSERT OR IGNORE INTO settings (key, value) VALUES ('secret', ?)",
                "SELECT value FROM settings WHERE key = 'secret'",
                &format!("candidate-{index}"),
            )
            .await
            .unwrap()
        }));
    }
    let mut values = Vec::new();
    for task in tasks {
        values.push(task.await.unwrap());
    }
    values.dedup();
    assert_eq!(values.len(), 1);
}
