use crate::config::AppConfig;
use crate::db::{socks5, wireguard as db_wg};
use crate::error::AppError;
use crate::service::cache::TtlCache;
use crate::sync::locks::NamedLockManager;
use crate::wireguard::peer_manager;
use crate::wireguard::server::WireGuardServer;
use crate::db::DbPool;
use std::sync::Arc;
use tracing::info;

/// Result of a lease allocation or extension.
#[derive(Debug, Clone)]
pub struct LeaseResult {
    /// The config content (text or JSON string).
    pub config_text: Option<String>,
    /// The config as JSON value (for JSON format responses).
    pub config_json: Option<serde_json::Value>,
    /// Lease reference (peer_id for WG, username for SOCKS5).
    pub lease_ref: String,
    /// Lease expiration timestamp in ms.
    pub lease_expires_at: i64,
}

/// Get a worker VPN config — new allocation or extension.
/// Mirrors `get_worker_config_as_worker` from `modules/api/worker.js`.
pub async fn get_worker_config(
    pool: &DbPool,
    locks: &NamedLockManager,
    cache: &TtlCache,
    config: &AppConfig,
    wg_server: &Option<Arc<WireGuardServer>>,
    lease_type: &str,
    lease_seconds: i64,
    priority: bool,
    format: &str,
    extend_ref: Option<&str>,
    extend_expires_at: Option<i64>,
) -> Result<LeaseResult, AppError> {
    // Extension branch
    if let Some(ref_val) = extend_ref {
        let expected = extend_expires_at.ok_or_else(|| {
            AppError::Validation("extend_expires_at is required when extend_ref is provided".into())
        })?;
        let new_expires_at = chrono::Utc::now().timestamp_millis() + lease_seconds * 1000;

        match lease_type {
            "wireguard" => {
                let peer_id: i32 = ref_val.parse().map_err(|_| {
                    AppError::Validation("Invalid extend_ref for wireguard: must be numeric".into())
                })?;
                info!("Extending WireGuard lease peer{} to {}", peer_id, new_expires_at);
                let result = db_wg::extend_wireguard_lease(pool, peer_id, expected, new_expires_at).await?;

                let server = wg_server.as_ref().ok_or_else(|| {
                    AppError::Internal("WireGuard server not available".into())
                })?;
                let peer_cfg = server.get_client_config(peer_id as u16).await?;
                let (config_text, config_json) = format_peer_config(&peer_cfg, format);

                Ok(LeaseResult {
                    config_text,
                    config_json,
                    lease_ref: peer_id.to_string(),
                    lease_expires_at: result.expires_at,
                })
            }
            "socks5" => {
                let username = ref_val;
                info!("Extending SOCKS5 lease {} to {}", username, new_expires_at);
                let result = socks5::extend_socks5_lease(pool, username, expected, new_expires_at).await?;

                let sock = socks5::read_socks5_config_by_username(pool, username)
                    .await?
                    .ok_or_else(|| {
                        AppError::Internal(format!(
                            "SOCKS5 config not found for {} after extension",
                            username
                        ))
                    })?;

                let (config_text, config_json) = format_socks5_config(&sock, format);

                Ok(LeaseResult {
                    config_text,
                    config_json,
                    lease_ref: username.to_string(),
                    lease_expires_at: result.expires_at,
                })
            }
            _ => Err(AppError::Validation(format!(
                "Unsupported type for lease extension: {}",
                lease_type
            ))),
        }
    } else {
        // New lease branch
        match lease_type {
            "wireguard" => allocate_wireguard(pool, locks, cache, config, wg_server, lease_seconds, priority, format).await,
            "socks5" => allocate_socks5(pool, locks, cache, config, lease_seconds, priority, format).await,
            _ => Err(AppError::Validation(format!(
                "Invalid type: {}. Must be one of 'wireguard', 'socks5'",
                lease_type
            ))),
        }
    }
}

/// Allocate a new WireGuard lease using the embedded WireGuard server.
async fn allocate_wireguard(
    pool: &DbPool,
    locks: &NamedLockManager,
    _cache: &TtlCache,
    config: &AppConfig,
    wg_server: &Option<Arc<WireGuardServer>>,
    lease_seconds: i64,
    _priority: bool,
    format: &str,
) -> Result<LeaseResult, AppError> {
    let peer_count = config.effective_peer_count() as i32;
    let start_id = 1;
    let end_id = peer_count;
    let expires_at = chrono::Utc::now().timestamp_millis() + lease_seconds * 1000;

    // CI mock mode
    if config.ci_mock_wg_container {
        let lease = db_wg::register_wireguard_lease(pool, locks, start_id, end_id, expires_at).await?;
        let mock_config = serde_json::json!({
            "interface": { "Address": "10.0.0.2/32", "PrivateKey": "mock", "DNS": "1.1.1.1" },
            "peer": { "PublicKey": "mock", "PresharedKey": "mock", "AllowedIPs": "0.0.0.0/0, ::/0", "Endpoint": "127.0.0.1:51820" }
        });
        return Ok(LeaseResult {
            config_text: Some("[Interface]\nAddress = 10.0.0.2/32\nPrivateKey = mock\nDNS = 1.1.1.1\n\n[Peer]\nPublicKey = mock\nPresharedKey = mock\nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = 127.0.0.1:51820".into()),
            config_json: Some(mock_config),
            lease_ref: lease.peer_id.to_string(),
            lease_expires_at: expires_at,
        });
    }

    let server = wg_server.as_ref().ok_or_else(|| {
        AppError::Internal("WireGuard server not available".into())
    })?;

    // Allocate peer: reserve DB slot + add to WG interface
    let allocation =
        peer_manager::allocate_peer(server, pool, locks, start_id, end_id, expires_at).await?;

    let (config_text, config_json) = format_peer_config(&allocation.peer_config, format);
    info!(
        "Allocated WireGuard peer {}, expires at {}",
        allocation.peer_id, expires_at
    );

    Ok(LeaseResult {
        config_text,
        config_json,
        lease_ref: allocation.peer_id.to_string(),
        lease_expires_at: allocation.expires_at,
    })
}

/// Allocate a new SOCKS5 lease.
async fn allocate_socks5(
    pool: &DbPool,
    locks: &NamedLockManager,
    _cache: &TtlCache,
    config: &AppConfig,
    lease_seconds: i64,
    priority: bool,
    format: &str,
) -> Result<LeaseResult, AppError> {
    let expires_at = chrono::Utc::now().timestamp_millis() + lease_seconds * 1000;
    let priority_slots = config.priority_slots;

    // For non-priority, check availability and refresh if needed
    if !priority {
        let count = socks5::count_available_socks(pool, priority_slots).await?;
        info!(
            "Available socks (after skipping {} priority slots): {}",
            priority_slots, count
        );

        if count == 0 {
            info!("No available socks, cleaning up expired leases");
            socks5::cleanup_expired_socks5_configs(pool).await?;
            let new_count = socks5::count_available_socks(pool, priority_slots).await?;

            if new_count == 0 {
                return Err(AppError::LeaseUnavailable(
                    "No available SOCKS5 slots after cleanup".into(),
                ));
            }
        }
    } else {
        info!(
            "Priority SOCKS5 request, using shared priority pool ({} slots)",
            priority_slots
        );
    }

    let sock = socks5::get_socks5_config(pool, locks, expires_at, priority, priority_slots).await?;

    let (config_text, config_json) = format_socks5_config(&sock, format);

    info!(
        "Leased {}SOCKS5 config: {}@{}:{}",
        if priority { "priority " } else { "" },
        sock.username,
        sock.ip_address,
        sock.port
    );

    Ok(LeaseResult {
        config_text,
        config_json,
        lease_ref: sock.username.clone(),
        lease_expires_at: expires_at,
    })
}

fn format_peer_config(
    peer_cfg: &crate::wireguard::server::PeerConfig,
    format: &str,
) -> (Option<String>, Option<serde_json::Value>) {
    match format {
        "text" => (Some(peer_cfg.to_text()), None),
        _ => (None, Some(peer_cfg.to_json())),
    }
}

fn format_socks5_config(
    sock: &socks5::Socks5Config,
    format: &str,
) -> (Option<String>, Option<serde_json::Value>) {
    match format {
        "text" => {
            let text = format!(
                "socks5://{}:{}@{}:{}",
                sock.username, sock.password, sock.ip_address, sock.port
            );
            (Some(text), None)
        }
        _ => {
            let json = serde_json::json!({
                "username": sock.username,
                "password": sock.password,
                "ip_address": sock.ip_address,
                "port": sock.port
            });
            (None, Some(json))
        }
    }
}
