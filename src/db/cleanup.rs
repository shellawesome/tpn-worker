use crate::config::{AppConfig, RunMode};
use crate::db::{wireguard as db_wg, DbPool};
use crate::wireguard::server::WireGuardServer;
use std::sync::Arc;
use tracing::{error, info, warn};

/// Staleness thresholds in milliseconds.
const STALE_1_YEAR_MS: i64 = 365 * 24 * 60 * 60 * 1000;

/// Periodic cleanup of stale database records.
/// For WireGuard peers, removes from the kernel interface before deleting DB rows.
pub async fn database_cleanup(
    pool: &DbPool,
    config: &AppConfig,
    wg_server: &Option<Arc<WireGuardServer>>,
) {
    let now_ms = chrono::Utc::now().timestamp_millis();

    // Timestamps table (all modes): 1 year staleness
    cleanup_table(pool, "timestamps", "updated", now_ms - STALE_1_YEAR_MS).await;

    // Worker mode specific
    if config.run_mode == RunMode::Worker {
        // 1. Remove expired peers from WireGuard kernel interface
        if let Some(server) = wg_server {
            match db_wg::expired_peer_ids(pool).await {
                Ok(ids) => {
                    for id in &ids {
                        if let Err(e) = server.remove_peer(*id as u16).await {
                            warn!(
                                "Failed to remove expired WG peer {} from interface: {}",
                                id, e
                            );
                        }
                    }
                    if !ids.is_empty() {
                        info!(
                            "Removed {} expired peers from WireGuard interface",
                            ids.len()
                        );
                    }
                }
                Err(e) => warn!("Failed to query expired WG peers: {}", e),
            }
        }

        // 2. Then delete DB rows immediately once the lease expires.
        cleanup_table(pool, "worker_wireguard_configs", "expires_at", now_ms).await;
    }
}

async fn cleanup_table(pool: &DbPool, table: &str, time_field: &str, cutoff: i64) {
    let query = format!("DELETE FROM {} WHERE {} < $1", table, time_field);
    match sqlx::query(&query).bind(cutoff).execute(pool).await {
        Ok(result) => {
            let count = result.rows_affected();
            if count > 0 {
                info!("Cleaned up {} stale rows from {}", count, table);
            }
        }
        Err(e) => {
            // Table may not exist in this run mode — that's fine
            if !e.to_string().contains("no such table") {
                error!("Error cleaning up {}: {}", table, e);
            }
        }
    }
}
