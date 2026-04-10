use crate::db::DbPool;
use crate::error::AppError;
use crate::sync::locks::NamedLockManager;
use sqlx::Row;
use std::time::Duration;
use tracing::info;

/// WireGuard lease record.
#[derive(Debug, Clone)]
pub struct WgLease {
    pub peer_id: i32,
    pub expires_at: i64,
}

/// Allocate a WireGuard peer ID within [start_id, end_id] range.
/// Uses a recursive CTE to find the first available ID, under a named lock.
pub async fn register_wireguard_lease(
    pool: &DbPool,
    locks: &NamedLockManager,
    start_id: i32,
    end_id: i32,
    expires_at: i64,
) -> Result<WgLease, AppError> {
    locks
        .with_lock(
            "register_wireguard_lease",
            Duration::from_secs(60),
            || async {
                // First attempt: find a free slot
                if let Some(lease) = try_allocate(pool, start_id, end_id, expires_at).await? {
                    return Ok(lease);
                }

                // Pool exhausted — clean up expired and retry
                info!("All WireGuard slots occupied, cleaning up expired configs...");
                cleanup_expired_wireguard_configs(pool).await?;

                if let Some(lease) = try_allocate(pool, start_id, end_id, expires_at).await? {
                    return Ok(lease);
                }

                // Still full — report earliest expiry for diagnostics
                let earliest: Option<i64> =
                    sqlx::query_scalar("SELECT MIN(expires_at) FROM worker_wireguard_configs")
                        .fetch_one(pool)
                        .await
                        .ok();

                Err(AppError::LeaseUnavailable(format!(
                    "All WireGuard peer slots ({}-{}) exhausted. Earliest expiry: {:?}",
                    start_id, end_id, earliest
                )))
            },
        )
        .await
}

/// Try to allocate a single free peer ID. Returns None if all slots occupied.
async fn try_allocate(
    pool: &DbPool,
    start_id: i32,
    end_id: i32,
    expires_at: i64,
) -> Result<Option<WgLease>, AppError> {
    // Find first available ID using recursive CTE (replaces PG generate_series)
    let available_id: Option<i32> = sqlx::query_scalar(
        "WITH RECURSIVE ids(id) AS (
             SELECT $1
             UNION ALL
             SELECT id + 1 FROM ids WHERE id < $2
         )
         SELECT id FROM ids
         WHERE id NOT IN (SELECT id FROM worker_wireguard_configs)
         ORDER BY id ASC
         LIMIT 1",
    )
    .bind(start_id)
    .bind(end_id)
    .fetch_optional(pool)
    .await?;

    let peer_id = match available_id {
        Some(id) => id,
        None => return Ok(None),
    };

    // Insert or update the lease
    let now_str = chrono::Utc::now().to_rfc3339();
    sqlx::query(
        "INSERT INTO worker_wireguard_configs (id, expires_at, updated_at)
         VALUES ($1, $2, $3)
         ON CONFLICT (id) DO UPDATE SET expires_at = $2, updated_at = $3",
    )
    .bind(peer_id)
    .bind(expires_at)
    .bind(&now_str)
    .execute(pool)
    .await?;

    info!(
        "Allocated WireGuard peer {} (expires_at: {})",
        peer_id, expires_at
    );
    Ok(Some(WgLease {
        peer_id,
        expires_at,
    }))
}

/// Extend an existing WireGuard lease with optimistic locking.
pub async fn extend_wireguard_lease(
    pool: &DbPool,
    peer_id: i32,
    expected_expires_at: i64,
    new_expires_at: i64,
) -> Result<WgLease, AppError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let now_str = chrono::Utc::now().to_rfc3339();

    let result = sqlx::query(
        "UPDATE worker_wireguard_configs
         SET expires_at = $1, updated_at = $2
         WHERE id = $3 AND expires_at = $4 AND expires_at > $5",
    )
    .bind(new_expires_at)
    .bind(&now_str)
    .bind(peer_id)
    .bind(expected_expires_at)
    .bind(now_ms)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::LeaseUnavailable(format!(
            "WireGuard lease extension failed for peer {}: lease not found, expired, or reallocated",
            peer_id
        )));
    }

    info!(
        "Extended WireGuard lease peer {} → {}",
        peer_id, new_expires_at
    );
    Ok(WgLease {
        peer_id,
        expires_at: new_expires_at,
    })
}

/// Delete a WireGuard lease with optional reallocation guard.
pub async fn mark_config_as_free(
    pool: &DbPool,
    peer_id: i32,
    expected_expires_at: Option<i64>,
) -> Result<(), AppError> {
    if let Some(expected) = expected_expires_at {
        sqlx::query("DELETE FROM worker_wireguard_configs WHERE id = $1 AND expires_at = $2")
            .bind(peer_id)
            .bind(expected)
            .execute(pool)
            .await?;
    } else {
        sqlx::query("DELETE FROM worker_wireguard_configs WHERE id = $1")
            .bind(peer_id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// List all active (non-expired) WireGuard leases.
pub async fn check_open_leases(pool: &DbPool) -> Vec<WgLease> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let rows = sqlx::query(
        "SELECT id, expires_at FROM worker_wireguard_configs
         WHERE expires_at > $1 ORDER BY expires_at ASC",
    )
    .bind(now_ms)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    rows.into_iter()
        .map(|row| WgLease {
            peer_id: row.get("id"),
            expires_at: row.get("expires_at"),
        })
        .collect()
}

/// Return IDs of expired WireGuard peers (for interface cleanup before DB deletion).
pub async fn expired_peer_ids(pool: &DbPool) -> Result<Vec<i32>, AppError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let ids: Vec<i32> =
        sqlx::query_scalar("SELECT id FROM worker_wireguard_configs WHERE expires_at < $1")
            .bind(now_ms)
            .fetch_all(pool)
            .await?;
    Ok(ids)
}

/// Remove expired WireGuard configs.
pub async fn cleanup_expired_wireguard_configs(pool: &DbPool) -> Result<u64, AppError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let result = sqlx::query("DELETE FROM worker_wireguard_configs WHERE expires_at < $1")
        .bind(now_ms)
        .execute(pool)
        .await?;
    let count = result.rows_affected();
    if count > 0 {
        info!("Cleaned up {} expired WireGuard configs", count);
    }
    Ok(count)
}
