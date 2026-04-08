use crate::db::wireguard as db_wg;
use crate::error::AppError;
use crate::sync::locks::NamedLockManager;
use crate::wireguard::server::{PeerConfig, WireGuardServer};
use crate::db::DbPool;
use tracing::info;

/// Result of a peer allocation.
pub struct PeerAllocation {
    pub peer_config: PeerConfig,
    pub peer_id: i32,
    pub expires_at: i64,
}

/// Allocate a new WireGuard peer: reserve a DB slot, then add to the WG interface.
pub async fn allocate_peer(
    server: &WireGuardServer,
    pool: &DbPool,
    locks: &NamedLockManager,
    start_id: i32,
    end_id: i32,
    expires_at: i64,
) -> Result<PeerAllocation, AppError> {
    // 1. Reserve a peer slot in the database
    let lease = db_wg::register_wireguard_lease(pool, locks, start_id, end_id, expires_at).await?;
    let peer_id = lease.peer_id;

    // 2. Add peer to WireGuard interface (generates keys)
    let peer_config = match server.add_peer(peer_id as u16).await {
        Ok(cfg) => cfg,
        Err(e) => {
            // Rollback DB allocation on WG failure
            let _ = db_wg::mark_config_as_free(pool, peer_id, Some(expires_at)).await;
            return Err(e);
        }
    };

    // 3. Store peer keys in the database for restart recovery
    if let Err(e) = store_peer_keys(
        pool,
        peer_id,
        &peer_config.client_private_key,
        &peer_config.preshared_key,
        &peer_config.client_address,
    )
    .await
    {
        // Rollback: keys must be persisted for restart recovery
        let _ = server.remove_peer(peer_id as u16).await;
        let _ = db_wg::mark_config_as_free(pool, peer_id, Some(expires_at)).await;
        return Err(AppError::Internal(format!("Failed to persist peer keys: {}", e)));
    }

    info!(
        "Allocated WireGuard peer {} (expires {})",
        peer_id, expires_at
    );

    Ok(PeerAllocation {
        peer_config,
        peer_id,
        expires_at,
    })
}

/// Release a WireGuard peer: remove from interface and free DB slot.
#[allow(dead_code)]
pub async fn release_peer(
    server: &WireGuardServer,
    pool: &DbPool,
    peer_id: u16,
) -> Result<(), AppError> {
    // Remove from WireGuard interface
    server.remove_peer(peer_id).await?;

    // Free the DB slot
    db_wg::mark_config_as_free(pool, peer_id as i32, None).await?;

    info!("Released WireGuard peer {}", peer_id);
    Ok(())
}

/// Store peer key material in the database for restart recovery.
async fn store_peer_keys(
    pool: &DbPool,
    peer_id: i32,
    private_key: &str,
    preshared_key: &str,
    allowed_ip: &str,
) -> Result<(), AppError> {
    // The client's public key (derived from private) is what the server stores
    let client_public_key =
        crate::wireguard::keygen::public_key_from_private(private_key)
            .map_err(|e| AppError::Internal(format!("Key derivation failed: {}", e)))?;

    sqlx::query(
        "UPDATE worker_wireguard_configs
         SET private_key = $1, public_key = $2, preshared_key = $3, allowed_ip = $4
         WHERE id = $5",
    )
    .bind(private_key)
    .bind(&client_public_key)
    .bind(preshared_key)
    .bind(allowed_ip)
    .bind(peer_id)
    .execute(pool)
    .await?;

    Ok(())
}
