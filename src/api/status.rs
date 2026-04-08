use axum::extract::State;
use axum::response::Json;
use serde_json::{json, Value};

use crate::db::wireguard as db_wg;
use crate::db::socks5;
use crate::AppState;

/// GET /api/stats — Worker status and statistics.
pub async fn stats(State(state): State<AppState>) -> Json<Value> {
    let config = &state.config;
    let uptime = chrono::Utc::now()
        .signed_duration_since(state.start_time)
        .num_seconds();

    // Count open WG leases
    let wg_leases = db_wg::check_open_leases(&state.db).await;

    // Count available SOCKS5
    let socks_available = socks5::count_available_socks(&state.db, config.priority_slots)
        .await
        .unwrap_or(0);

    Json(json!({
        "status": "ok",
        "mode": config.run_mode.to_string(),
        "uptime_seconds": uptime,
        "version": state.version,
        "wireguard": {
            "peer_count": config.effective_peer_count(),
            "active_leases": wg_leases.len(),
        },
        "socks5": {
            "priority_slots": config.priority_slots,
            "available_non_priority": socks_available,
        },
    }))
}
