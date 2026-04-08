use crate::db::DbPool;
use tracing::warn;

/// Insert a registration attempt record.
pub async fn insert_registration(
    pool: &DbPool,
    mining_pool_url: &str,
    success: bool,
    status_code: u16,
    response_body: &str,
    error_message: &str,
) {
    let now = chrono::Utc::now().to_rfc3339();
    let success_int: i32 = if success { 1 } else { 0 };

    if let Err(e) = sqlx::query(
        "INSERT INTO worker_registration_log
         (mining_pool_url, success, status_code, response_body, error_message, created_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(mining_pool_url)
    .bind(success_int)
    .bind(status_code as i32)
    .bind(response_body)
    .bind(error_message)
    .bind(&now)
    .execute(pool)
    .await
    {
        warn!("Failed to log registration attempt: {}", e);
    }

    // Keep only last 200 entries
    let _ = sqlx::query(
        "DELETE FROM worker_registration_log WHERE id NOT IN (
             SELECT id FROM worker_registration_log ORDER BY id DESC LIMIT 200
         )",
    )
    .execute(pool)
    .await;
}

/// Fetch recent registration log entries (newest first).
pub async fn recent_registrations(pool: &DbPool, limit: i32) -> Vec<serde_json::Value> {
    let rows = sqlx::query_as::<_, (i64, String, i32, i32, String, String, String)>(
        "SELECT id, mining_pool_url, success, status_code, response_body, error_message, created_at
         FROM worker_registration_log
         ORDER BY id DESC
         LIMIT $1",
    )
    .bind(limit)
    .fetch_all(pool)
    .await;

    match rows {
        Ok(rows) => rows
            .into_iter()
            .map(|(id, url, success, status, body, err, created)| {
                serde_json::json!({
                    "id": id,
                    "mining_pool_url": url,
                    "success": success == 1,
                    "status_code": status,
                    "response_body": body,
                    "error_message": err,
                    "created_at": created,
                })
            })
            .collect(),
        Err(e) => {
            warn!("Failed to fetch registration log: {}", e);
            vec![]
        }
    }
}
