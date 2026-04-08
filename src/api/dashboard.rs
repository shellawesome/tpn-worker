use axum::extract::State;
use axum::response::{Html, Json};
use serde_json::{json, Value};
use tracing::info;

use crate::db::{registration_log, socks5, wireguard as db_wg};
use crate::AppState;

/// GET /api/dashboard — Aggregated dashboard JSON data.
pub async fn dashboard_data(State(state): State<AppState>) -> Json<Value> {
    info!("GET /api/dashboard requested");
    let config = &state.config;
    let uptime_secs = chrono::Utc::now()
        .signed_duration_since(state.start_time)
        .num_seconds();

    // WireGuard stats
    let wg_leases = db_wg::check_open_leases(&state.db).await;
    let wg_active_peers = match &state.wg_server {
        Some(wg) => wg.active_peer_count().await,
        None => 0,
    };

    // SOCKS5 stats
    let socks_available = socks5::count_available_socks(&state.db, config.priority_slots)
        .await
        .unwrap_or(0);

    // Registration history
    let registrations = registration_log::recent_registrations(&state.db, 50).await;

    // Last registration status
    let last_reg = registrations.first();
    let last_reg_success = last_reg
        .and_then(|r| r.get("success"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let last_reg_time = last_reg
        .and_then(|r| r.get("created_at"))
        .and_then(|v| v.as_str())
        .unwrap_or("-");

    Json(json!({
        "worker": {
            "version": state.version,
            "mode": config.run_mode.to_string(),
            "uptime_seconds": uptime_secs,
            "start_time": state.start_time.to_rfc3339(),
            "git_branch": state.git_branch,
            "git_hash": state.git_hash,
        },
        "mining_pool": {
            "url": config.effective_mining_pool_url(),
            "name": config.mining_pool_name,
            "last_registration_success": last_reg_success,
            "last_registration_time": last_reg_time,
            "rewards_url": config.mining_pool_rewards,
            "website_url": config.mining_pool_website_url,
        },
        "wireguard": {
            "enabled": state.wg_server.is_some(),
            "interface": config.wg_interface_name,
            "max_peers": config.effective_peer_count(),
            "active_peers": wg_active_peers,
            "active_leases": wg_leases.len(),
            "listen_port": config.wireguard_server_port,
            "subnet": config.wg_subnet,
            "dns": config.wg_dns,
            "server_public_key": state.wg_server.as_ref().map(|wg| wg.server_public_key().to_string()).unwrap_or_default(),
        },
        "proxy": {
            "socks5_port": config.socks5_port,
            "http_proxy_port": config.http_proxy_port,
            "credential_count": config.proxy_credential_count,
            "active_credentials": state.credentials.credential_count(),
            "available_non_priority": socks_available,
            "priority_slots": config.priority_slots,
        },
        "network": {
            "public_host": config.server_public_host,
            "public_port": config.server_port,
            "protocol": config.server_public_protocol,
            "base_url": config.base_url(),
        },
        "payment": {
            "evm_address": config.payment_address_evm,
            "bittensor_address": config.payment_address_bittensor,
        },
        "database": {
            "path": config.sqlite_path,
        },
        "registration_history": registrations,
    }))
}

/// GET /dashboard — Embedded HTML dashboard page.
pub async fn dashboard_page() -> Html<&'static str> {
    info!("GET /dashboard requested — serving embedded HTML");
    Html(include_str!("../dashboard.html"))
}
