use axum::extract::State;
use axum::response::Json;
use crate::AppState;
use serde_json::{json, Value};

/// GET / — Health check endpoint.
/// Mirrors `routes/health.js` — field names must match exactly for mining pool validation.
pub async fn health(State(state): State<AppState>) -> Json<Value> {
    let config = &state.config;
    let mode = config.run_mode.to_string();

    let mut resp = json!({
        "notice": format!("I am a TPN Network {} component running v{}", mode, state.version),
        "info": "https://tpn.taofu.xyz",
        "version": state.version,
        "last_start": state.start_time.to_rfc3339(),
        "branch": state.git_branch,
        "hash": state.git_hash,
    });

    // Conditionally include fields (mirrors JS spread pattern)
    let obj = resp.as_object_mut().unwrap();
    if !config.mining_pool_name.is_empty() {
        obj.insert("MINING_POOL_NAME".into(), json!(config.mining_pool_name));
    }
    if !config.mining_pool_url.is_empty() {
        obj.insert("MINING_POOL_URL".into(), json!(config.effective_mining_pool_url()));
    }
    if !config.server_public_host.is_empty() {
        obj.insert("SERVER_PUBLIC_HOST".into(), json!(config.server_public_host));
    }
    obj.insert("SERVER_PUBLIC_PORT".into(), json!(config.server_port.to_string()));
    obj.insert("SERVER_PUBLIC_PROTOCOL".into(), json!(config.server_public_protocol));
    if !config.mining_pool_rewards.is_empty() {
        obj.insert("MINING_POOL_REWARDS".into(), json!(config.mining_pool_rewards));
    }
    if !config.mining_pool_website_url.is_empty() {
        obj.insert("MINING_POOL_WEBSITE_URL".into(), json!(config.mining_pool_website_url));
    }
    if !config.broadcast_message.is_empty() {
        obj.insert("BROADCAST_MESSAGE".into(), json!(config.broadcast_message));
    }
    if !config.contact_method.is_empty() {
        obj.insert("CONTACT_METHOD".into(), json!(config.contact_method));
    }

    Json(resp)
}

/// GET /ping — Simple ping endpoint.
pub async fn ping() -> &'static str {
    "pong"
}
