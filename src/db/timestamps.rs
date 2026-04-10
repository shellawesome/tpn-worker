#![allow(dead_code)]
use crate::db::DbPool;

/// Get a timestamp value by label.
pub async fn get_timestamp(pool: &DbPool, label: &str) -> Option<i64> {
    let result: Option<i64> =
        sqlx::query_scalar("SELECT timestamp FROM timestamps WHERE label = $1")
            .bind(label)
            .fetch_optional(pool)
            .await
            .ok()
            .flatten();
    result
}

/// Set a timestamp value (upsert).
pub async fn set_timestamp(pool: &DbPool, label: &str, value: i64) -> Result<(), sqlx::Error> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "INSERT INTO timestamps (label, timestamp, updated)
         VALUES ($1, $2, $3)
         ON CONFLICT (label) DO UPDATE SET timestamp = $2, updated = $3",
    )
    .bind(label)
    .bind(value)
    .bind(now_ms)
    .execute(pool)
    .await?;
    Ok(())
}
