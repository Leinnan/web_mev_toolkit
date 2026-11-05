//! The audit log of security-relevant actions.

use serde_json::Value;
use sqlx::Sqlite;

/// Appends an entry. Takes any executor, so pass the transaction applying the
/// change: the entry and the change then commit or roll back together, and a failed
/// entry can't go unnoticed.
///
/// # Errors
///
/// Returns the database error when the insert fails.
pub async fn record<'e, E>(
    executor: E,
    user_id: Option<i64>,
    action: &str,
    target_type: Option<&str>,
    target_id: Option<&str>,
    details: &Value,
) -> Result<(), sqlx::Error>
where
    E: sqlx::Executor<'e, Database = Sqlite>,
{
    sqlx::query(
        "INSERT INTO audit_log (user_id, action, target_type, target_id, details_json)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(user_id)
    .bind(action)
    .bind(target_type)
    .bind(target_id)
    .bind(details.to_string())
    .execute(executor)
    .await?;
    Ok(())
}
