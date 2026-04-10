use crate::config::AppConfig;
use crate::db::registration_log;
use crate::db::DbPool;
use crate::error::AppError;
use crate::service::cache::TtlCache;
use crate::service::lease_manager;
use crate::sync::locks::NamedLockManager;
use crate::wireguard::server::WireGuardServer;
use std::sync::Arc;
use tracing::{info, warn};

/// Register this worker with the mining pool.
/// Mirrors `register_with_mining_pool` from `modules/api/worker.js`.
pub async fn register_with_mining_pool(
    pool: &DbPool,
    locks: &NamedLockManager,
    cache: &TtlCache,
    config: &AppConfig,
    wg_server: &Option<Arc<WireGuardServer>>,
) -> Result<bool, AppError> {
    let mining_pool_url = config.effective_mining_pool_url();
    let public_url = config.base_url();

    info!("Registering with mining pool {}", mining_pool_url);

    // Build base worker object
    let mut worker = serde_json::json!({
        "mining_pool_url": mining_pool_url,
        "public_url": public_url,
        "public_port": config.server_port,
        "payment_address_evm": config.payment_address_evm,
        "payment_address_bittensor": config.payment_address_bittensor,
    });

    // Try to attach a WireGuard config (120s lease for registration probe)
    // Mining pool expects wireguard_config as text (INI format), not JSON
    match lease_manager::get_worker_config(
        pool,
        locks,
        cache,
        config,
        wg_server,
        "wireguard",
        120,
        false,
        "text",
        None,
        None,
    )
    .await
    {
        Ok(lease) => {
            if let Some(text_config) = lease.config_text {
                worker["wireguard_config"] = serde_json::Value::String(text_config);
            }
        }
        Err(e) => {
            warn!("Could not obtain WireGuard config for registration: {}", e);
        }
    }

    // Try to attach a SOCKS5 config
    // Mining pool expects text format: socks5://user:pass@ip:port
    match lease_manager::get_worker_config(
        pool, locks, cache, config, wg_server, "socks5", 120, false, "text", None, None,
    )
    .await
    {
        Ok(lease) => {
            if let Some(text_config) = lease.config_text {
                worker["socks5_config"] = serde_json::Value::String(text_config);
            }
        }
        Err(e) => {
            warn!("Could not obtain SOCKS5 config for registration: {}", e);
        }
    }

    // POST to mining pool
    let query = format!("{}/miner/broadcast/worker", mining_pool_url);
    info!("Posting worker registration to {}", query);
    info!(
        "Registration payload: {}",
        serde_json::to_string_pretty(&worker).unwrap_or_default()
    );

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|e| AppError::Internal(format!("HTTP client error: {}", e)))?;

    let response = client
        .post(&query)
        .json(&worker)
        .send()
        .await
        .map_err(|e| AppError::Internal(format!("Mining pool registration failed: {}", e)))?;

    let status = response.status();
    let body_text = response
        .text()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read mining pool response: {}", e)))?;

    info!("Mining pool response (status {}): {}", status, body_text);

    let body: serde_json::Value = serde_json::from_str(&body_text)
        .map_err(|e| AppError::Internal(format!("Failed to parse mining pool response: {}", e)))?;

    let registered = body
        .get("registered")
        .and_then(|v| v.as_bool())
        .or_else(|| body.get("success").and_then(|v| v.as_bool()))
        .unwrap_or(false);

    let error_msg = body.get("error").and_then(|v| v.as_str()).unwrap_or("");
    if !error_msg.is_empty() {
        warn!("Error registering with mining pool: {}", error_msg);
    } else {
        info!("Registered with mining pool: {}", registered);
    }

    // Log registration attempt to DB
    registration_log::insert_registration(
        pool,
        &mining_pool_url,
        registered,
        status.as_u16(),
        &body_text,
        error_msg,
    )
    .await;

    Ok(registered)
}
