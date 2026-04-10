use crate::db::DbPool;
use crate::error::AppError;
use crate::sync::locks::NamedLockManager;
use rand::seq::IndexedRandom;
use sqlx::FromRow;
use std::time::Duration;
use tracing::info;

/// SOCKS5 proxy configuration record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, FromRow)]
pub struct Socks5Config {
    pub id: i32,
    pub ip_address: String,
    pub port: i32,
    pub username: String,
    pub password: String,
    pub available: bool,
    pub expires_at: i64,
    pub updated: i64,
}

/// SOCKS5 lease extension result.
#[derive(Debug, Clone)]
pub struct Socks5LeaseExtension {
    #[allow(dead_code)]
    pub username: String,
    pub expires_at: i64,
}

/// Allocate a SOCKS5 config (priority or non-priority).
/// Mirrors `get_socks5_config` from `modules/database/worker_socks5.js`.
pub async fn get_socks5_config(
    pool: &DbPool,
    locks: &NamedLockManager,
    expires_at: i64,
    priority: bool,
    priority_slots: i64,
) -> Result<Socks5Config, AppError> {
    if priority {
        get_socks5_config_priority(pool, expires_at, priority_slots).await
    } else {
        get_socks5_config_non_priority(pool, locks, expires_at, priority_slots).await
    }
}

/// Priority pool: shared slots (first N), never marked unavailable.
/// Random selection for load distribution.
async fn get_socks5_config_priority(
    pool: &DbPool,
    expires_at: i64,
    priority_slots: i64,
) -> Result<Socks5Config, AppError> {
    let configs: Vec<Socks5Config> = sqlx::query_as(
        "SELECT id, ip_address, port, username, password, available, expires_at, updated
         FROM worker_socks5_configs
         WHERE available = TRUE
         ORDER BY id ASC
         LIMIT $1",
    )
    .bind(priority_slots)
    .fetch_all(pool)
    .await?;

    if configs.is_empty() {
        return Err(AppError::LeaseUnavailable(
            "No priority SOCKS5 configs available".to_string(),
        ));
    }

    // Random selection from priority pool
    let config = {
        let mut rng = rand::rng();
        configs.choose(&mut rng).unwrap().clone()
    };

    // Update expires_at (don't change available flag for priority slots)
    let now_ms = chrono::Utc::now().timestamp_millis();
    sqlx::query(
        "UPDATE worker_socks5_configs SET expires_at = $1, updated = $2 WHERE username = $3",
    )
    .bind(expires_at)
    .bind(now_ms)
    .bind(&config.username)
    .execute(pool)
    .await?;

    info!("Allocated priority SOCKS5 config: {}", config.username);
    Ok(config)
}

/// Non-priority pool: exclusive allocation (after offset), marked unavailable.
async fn get_socks5_config_non_priority(
    pool: &DbPool,
    locks: &NamedLockManager,
    expires_at: i64,
    priority_slots: i64,
) -> Result<Socks5Config, AppError> {
    locks
        .with_lock("get_socks5_config", Duration::from_secs(30), || async {
            // Try to find an available config after the priority slots
            let config: Option<Socks5Config> = sqlx::query_as(
                "SELECT id, ip_address, port, username, password, available, expires_at, updated
                 FROM worker_socks5_configs
                 WHERE available = TRUE
                 ORDER BY id ASC
                 LIMIT 1 OFFSET $1",
            )
            .bind(priority_slots)
            .fetch_optional(pool)
            .await?;

            let config = match config {
                Some(c) => c,
                None => {
                    // Clean up expired and retry
                    cleanup_expired_socks5_configs(pool).await?;

                    let retried: Option<Socks5Config> = sqlx::query_as(
                        "SELECT id, ip_address, port, username, password, available, expires_at, updated
                         FROM worker_socks5_configs
                         WHERE available = TRUE
                         ORDER BY id ASC
                         LIMIT 1 OFFSET $1",
                    )
                    .bind(priority_slots)
                    .fetch_optional(pool)
                    .await?;

                    retried.ok_or_else(|| {
                        AppError::LeaseUnavailable(
                            "No non-priority SOCKS5 configs available".to_string(),
                        )
                    })?
                }
            };

            // Mark as unavailable (exclusive lease)
            let now_ms = chrono::Utc::now().timestamp_millis();
            sqlx::query(
                "UPDATE worker_socks5_configs
                 SET available = FALSE, expires_at = $1, updated = $2
                 WHERE username = $3 AND password = $4",
            )
            .bind(expires_at)
            .bind(now_ms)
            .bind(&config.username)
            .bind(&config.password)
            .execute(pool)
            .await?;

            info!("Allocated non-priority SOCKS5 config: {}", config.username);
            Ok(config)
        })
        .await
}

/// Extend a SOCKS5 lease with optimistic locking.
/// Mirrors `extend_socks5_lease` from `modules/database/worker_socks5.js`.
pub async fn extend_socks5_lease(
    pool: &DbPool,
    username: &str,
    expected_expires_at: i64,
    new_expires_at: i64,
) -> Result<Socks5LeaseExtension, AppError> {
    let now_ms = chrono::Utc::now().timestamp_millis();

    let result = sqlx::query(
        "UPDATE worker_socks5_configs
         SET expires_at = $1, updated = $2
         WHERE username = $3 AND expires_at = $4 AND expires_at > $5",
    )
    .bind(new_expires_at)
    .bind(now_ms)
    .bind(username)
    .bind(expected_expires_at)
    .bind(now_ms)
    .execute(pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::LeaseUnavailable(format!(
            "SOCKS5 lease extension failed for {}: not found, expired, or reallocated",
            username
        )));
    }

    info!("Extended SOCKS5 lease {} → {}", username, new_expires_at);
    Ok(Socks5LeaseExtension {
        username: username.to_string(),
        expires_at: new_expires_at,
    })
}

/// Read a SOCKS5 config by username.
pub async fn read_socks5_config_by_username(
    pool: &DbPool,
    username: &str,
) -> Result<Option<Socks5Config>, AppError> {
    let config: Option<Socks5Config> = sqlx::query_as(
        "SELECT id, ip_address, port, username, password, available, expires_at, updated
         FROM worker_socks5_configs WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    Ok(config)
}

/// Batch upsert SOCKS5 configs to database.
/// Mirrors `write_socks` from `modules/database/worker_socks5.js`.
pub async fn write_socks(pool: &DbPool, socks: &[Socks5Config]) -> Result<(), AppError> {
    if socks.is_empty() {
        return Ok(());
    }

    let now_ms = chrono::Utc::now().timestamp_millis();

    for sock in socks {
        sqlx::query(
            "INSERT INTO worker_socks5_configs (ip_address, port, username, password, available, updated, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (username) DO UPDATE SET
                password = EXCLUDED.password,
                updated = EXCLUDED.updated",
        )
        .bind(&sock.ip_address)
        .bind(sock.port)
        .bind(&sock.username)
        .bind(&sock.password)
        .bind(sock.available)
        .bind(now_ms)
        .bind(sock.expires_at)
        .execute(pool)
        .await?;
    }

    // Delete configs not in the current batch
    let usernames: Vec<String> = socks.iter().map(|s| s.username.clone()).collect();
    if !usernames.is_empty() {
        let placeholders: Vec<String> = (1..=usernames.len()).map(|i| format!("${}", i)).collect();
        let sql = format!(
            "DELETE FROM worker_socks5_configs WHERE username NOT IN ({})",
            placeholders.join(", ")
        );
        let mut query = sqlx::query(&sql);
        for username in &usernames {
            query = query.bind(username);
        }
        query.execute(pool).await?;
    }

    info!("Wrote {} SOCKS5 configs to database", socks.len());
    Ok(())
}

/// Clean up expired SOCKS5 configs: mark as available and reset expires_at.
pub async fn cleanup_expired_socks5_configs(pool: &DbPool) -> Result<u64, AppError> {
    let now_ms = chrono::Utc::now().timestamp_millis();
    let result = sqlx::query(
        "UPDATE worker_socks5_configs
         SET available = TRUE, expires_at = 0
         WHERE expires_at > 0 AND expires_at <= $1",
    )
    .bind(now_ms)
    .execute(pool)
    .await?;

    let count = result.rows_affected();
    if count > 0 {
        info!("Cleaned up {} expired SOCKS5 configs", count);
    }
    Ok(count)
}

/// Count available non-priority SOCKS5 configs.
pub async fn count_available_socks(pool: &DbPool, skip_slots: i64) -> Result<i64, AppError> {
    let count: Option<i64> = sqlx::query_scalar(
        "SELECT COUNT(*) FROM (
            SELECT id FROM worker_socks5_configs
            WHERE available = TRUE
            ORDER BY id ASC
            LIMIT -1 OFFSET $1
        ) AS non_priority_socks",
    )
    .bind(skip_slots)
    .fetch_one(pool)
    .await?;
    Ok(count.unwrap_or(0))
}
