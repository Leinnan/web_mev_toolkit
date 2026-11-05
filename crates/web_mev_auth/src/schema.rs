//! The tables this crate needs. One database has one list of applied migrations,
//! so a crate's own migrations would collide with the app's. The app copies
//! [`REFERENCE_SQL`] into a migration of its own instead, and [`verify_schema`]
//! catches drift at boot rather than on the first login.

use sqlx::SqlitePool;

/// SQL creating the tables, for the app to copy into a migration after `users`.
pub const REFERENCE_SQL: &str = include_str!("../sql/auth_tables.sql");

/// Column name, type, `NOT NULL`, and position in the primary key.
type Column = (&'static str, &'static str, bool, i64);

const TABLES: &[(&str, &[Column])] = &[
    (
        "sessions",
        &[
            ("id", "INTEGER", false, 1),
            ("token_hash", "TEXT", true, 0),
            ("user_id", "INTEGER", true, 0),
            ("csrf_token", "TEXT", true, 0),
            ("expires_at", "INTEGER", true, 0),
            ("created_at", "INTEGER", true, 0),
        ],
    ),
    (
        "login_throttle",
        &[
            ("identity_hash", "TEXT", false, 1),
            ("failures", "INTEGER", true, 0),
            ("locked_until", "INTEGER", false, 0),
            ("updated_at", "INTEGER", true, 0),
        ],
    ),
    (
        "bootstrap_lock",
        &[
            ("id", "INTEGER", false, 1),
            ("touched_at", "INTEGER", true, 0),
        ],
    ),
    (
        "audit_log",
        &[
            ("id", "INTEGER", false, 1),
            ("user_id", "INTEGER", false, 0),
            ("action", "TEXT", true, 0),
            ("target_type", "TEXT", false, 0),
            ("target_id", "TEXT", false, 0),
            ("details_json", "TEXT", true, 0),
            ("created_at", "INTEGER", true, 0),
        ],
    ),
];

/// Why the schema can't be used.
#[derive(Debug, thiserror::Error)]
pub enum SchemaError {
    /// The tables don't match what this crate needs.
    #[error("the auth tables don't match web_mev_auth: {}", .0.join("; "))]
    Mismatch(Vec<String>),
    /// A database error.
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

/// Checks that the tables have the columns this crate reads and writes. Defaults
/// aren't compared, and an app's extra nullable columns are fine.
///
/// # Errors
///
/// Returns [`SchemaError::Mismatch`], listing every difference.
pub async fn verify_schema(db: &SqlitePool) -> Result<(), SchemaError> {
    let mut problems = Vec::new();
    for (table, columns) in TABLES {
        let found: Vec<(String, String, i64, i64)> =
            sqlx::query_as(r#"SELECT name, type, "notnull", pk FROM pragma_table_info(?)"#)
                .bind(table)
                .fetch_all(db)
                .await?;
        if found.is_empty() {
            problems.push(format!("table {table} is missing"));
            continue;
        }
        for (name, kind, not_null, pk) in *columns {
            let Some((_, found_kind, found_not_null, found_pk)) =
                found.iter().find(|column| column.0 == *name)
            else {
                problems.push(format!("{table}.{name} is missing"));
                continue;
            };
            if !found_kind.eq_ignore_ascii_case(kind) {
                problems.push(format!("{table}.{name} is {found_kind}, expected {kind}"));
            }
            if (*found_not_null != 0) != *not_null {
                let expected = if *not_null { "NOT NULL" } else { "nullable" };
                problems.push(format!("{table}.{name} should be {expected}"));
            }
            if found_pk != pk {
                problems.push(format!("{table}.{name} has the wrong primary key position"));
            }
        }
    }
    if !token_hash_is_unique(db).await? {
        problems.push("sessions.token_hash is not UNIQUE".to_owned());
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(SchemaError::Mismatch(problems))
    }
}

async fn token_hash_is_unique(db: &SqlitePool) -> Result<bool, sqlx::Error> {
    let count: i64 = sqlx::query_scalar(
        r#"SELECT count(*) FROM pragma_index_list('sessions') AS list
           WHERE list."unique" = 1
             AND (SELECT count(*) FROM pragma_index_info(list.name)) = 1
             AND (SELECT name FROM pragma_index_info(list.name)) = 'token_hash'"#,
    )
    .fetch_one(db)
    .await?;
    Ok(count > 0)
}
