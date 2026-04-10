use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use std::net::SocketAddr;
use tracing::{debug, info, warn};

use crate::crypto::lease_token;
use crate::error::AppError;
use crate::net::dns::resolve_domain_to_ipv4s;
use crate::net::ip::{is_local_ip, sanitize_ipv4};
use crate::service::lease_manager;
use crate::AppState;

#[derive(Debug, Deserialize)]
pub struct LeaseParams {
    pub lease_seconds: Option<String>,
    pub lease_minutes: Option<String>,
    pub format: Option<String>,
    #[serde(rename = "type")]
    pub lease_type: Option<String>,
    pub priority: Option<String>,
    #[allow(dead_code)]
    pub feedback_url: Option<String>,
    pub extend_ref: Option<String>,
    pub extend_expires_at: Option<String>,
    pub lease_token: Option<String>,
}

/// GET /api/lease/new (also aliased as /api/config/new)
/// Core lease endpoint — mirrors `routes/api/lease.js`.
#[axum::debug_handler]
pub async fn lease_new(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(params): Query<LeaseParams>,
    State(state): State<AppState>,
) -> Result<Response, AppError> {
    let config = &state.config;

    // Worker access control: only accept requests from mining pool IP
    if config.is_worker() && !config.ci_mock_worker_responses {
        let mining_pool_url = config.effective_mining_pool_url();
        info!("Checking if caller is mining pool: {}", mining_pool_url);

        let caller_ip = sanitize_ipv4(&addr.ip().to_string());
        let allowed_pool_ips = resolve_domain_to_ipv4s(&mining_pool_url, &state.cache)
            .await
            .unwrap_or_default();
        let local_request = is_local_ip(
            &caller_ip,
            &config.tpn_internal_subnet,
            &config.tpn_external_subnet,
        );
        let ip_match = allowed_pool_ips.iter().any(|ip| ip == &caller_ip);

        debug!(
            "Worker lease request from {}, allowed pool IPs: {:?}, local_request: {}, match: {}",
            caller_ip, allowed_pool_ips, local_request, ip_match
        );

        if !ip_match && !local_request {
            warn!(
                "Access denied: {} does not match allowed mining pool IPs {:?}",
                caller_ip, allowed_pool_ips
            );
            return Err(AppError::Unauthorized(format!(
                "Worker does not accept lease requests from {}",
                caller_ip
            )));
        }
    }

    // Parse and validate parameters
    let format = params.format.as_deref().unwrap_or("json");
    let lease_type = params.lease_type.as_deref().unwrap_or("wireguard");

    // Backwards compat: lease_minutes → lease_seconds
    let lease_seconds: i64 = if let Some(ref ls) = params.lease_seconds {
        ls.parse().unwrap_or(0)
    } else if let Some(ref lm) = params.lease_minutes {
        let mins: f64 = lm.parse().unwrap_or(0.0);
        (mins * 60.0) as i64
    } else {
        0
    };

    let priority = params.priority.as_deref() == Some("true");

    // Validate inputs
    if lease_seconds <= 0 {
        return Err(AppError::Validation(format!(
            "Invalid lease_seconds: {}. Must be a valid number greater than 0.",
            lease_seconds
        )));
    }
    if !["json", "text"].contains(&format) {
        return Err(AppError::Validation(format!(
            "Invalid format: {}. Must be one of 'json', 'text'",
            format
        )));
    }
    if !["wireguard", "socks5"].contains(&lease_type) {
        return Err(AppError::Validation(format!(
            "Invalid type: {}. Must be one of 'wireguard', 'socks5'",
            lease_type
        )));
    }
    if params.extend_ref.is_some() && params.extend_expires_at.is_none() {
        return Err(AppError::Validation(
            "extend_expires_at is required when extend_ref is provided".into(),
        ));
    }

    // Verify lease token on extension requests
    if let Some(ref extend_ref) = params.extend_ref {
        let token = params.lease_token.as_deref().unwrap_or("");
        if !lease_token::verify_lease_token(extend_ref, token, &config.lease_token_secret) {
            return Err(AppError::Unauthorized(
                "Invalid or missing lease_token for lease extension".into(),
            ));
        }
    }

    let extend_expires_at = params
        .extend_expires_at
        .as_ref()
        .and_then(|v| v.parse::<i64>().ok());

    // Get config
    let result = lease_manager::get_worker_config(
        &state.db,
        &state.locks,
        &state.cache,
        config,
        &state.wg_server,
        lease_type,
        lease_seconds,
        priority,
        format,
        params.extend_ref.as_deref(),
        extend_expires_at,
    )
    .await?;

    // Build response headers
    let mut headers = HeaderMap::new();
    if let Ok(v) = result.lease_ref.parse() {
        headers.insert("X-Lease-Ref", v);
    }
    if let Ok(v) = result.lease_expires_at.to_string().parse() {
        headers.insert("X-Lease-Expires", v);
    }
    // Sign and return lease token so the caller can use it for extensions
    let token = lease_token::sign_lease_token(&result.lease_ref, &config.lease_token_secret);
    if let Ok(v) = token.parse() {
        headers.insert("X-Lease-Token", v);
    }

    // Return response
    if format == "text" {
        let body = result.config_text.unwrap_or_default();
        Ok((StatusCode::OK, headers, body).into_response())
    } else {
        let body = result.config_json.unwrap_or_else(|| json!({}));
        Ok((StatusCode::OK, headers, axum::Json(body)).into_response())
    }
}
