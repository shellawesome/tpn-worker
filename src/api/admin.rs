use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::Deserialize;
use std::path::PathBuf;

use crate::AppState;

#[derive(Deserialize)]
pub struct ConfigUpdateRequest {
    pub mining_pool_url: Option<String>,
    pub payment_address_evm: Option<String>,
    pub payment_address_bittensor: Option<String>,
    pub server_public_port: Option<u16>,
    pub socks5_port: Option<u16>,
    pub http_proxy_port: Option<u16>,
    pub wireguard_server_port: Option<u16>,
}

fn validate_port(port: u16, name: &str) -> Result<(), String> {
    if port == 0 {
        return Err(format!("{}: 端口不能为 0", name));
    }
    Ok(())
}

/// Return the .env file path
fn env_path() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home)
        .join(".config")
        .join("tpn-worker")
        .join(".env")
}

/// Update a single key=value in the .env file.
/// If the key exists (commented or not), replace the line. Otherwise append it.
pub fn update_env_value(key: &str, value: &str) -> std::io::Result<()> {
    let path = env_path();
    let content = std::fs::read_to_string(&path)?;

    let mut found = false;
    let mut lines: Vec<String> = content
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            // Match KEY=... or # KEY=...
            if trimmed.starts_with(&format!("{}=", key))
                || trimmed.starts_with(&format!("# {}=", key))
            {
                found = true;
                format!("{}={}", key, value)
            } else {
                line.to_string()
            }
        })
        .collect();

    if !found {
        lines.push(format!("{}={}", key, value));
    }

    let new_content = lines.join("\n");
    // Preserve trailing newline
    let new_content = if content.ends_with('\n') && !new_content.ends_with('\n') {
        format!("{}\n", new_content)
    } else {
        new_content
    };

    std::fs::write(&path, new_content)
}

/// POST /api/config/update — Update MINING_POOL_URL, PAYMENT_ADDRESS_EVM, PAYMENT_ADDRESS_BITTENSOR
pub async fn update_config(
    State(_state): State<AppState>,
    Json(req): Json<ConfigUpdateRequest>,
) -> impl IntoResponse {
    let mut updated = Vec::new();
    let mut errors = Vec::new();

    if let Some(ref v) = req.mining_pool_url {
        match update_env_value("MINING_POOL_URL", v) {
            Ok(_) => updated.push("MINING_POOL_URL"),
            Err(e) => errors.push(format!("MINING_POOL_URL: {}", e)),
        }
    }
    if let Some(ref v) = req.payment_address_evm {
        match update_env_value("PAYMENT_ADDRESS_EVM", v) {
            Ok(_) => updated.push("PAYMENT_ADDRESS_EVM"),
            Err(e) => errors.push(format!("PAYMENT_ADDRESS_EVM: {}", e)),
        }
    }
    if let Some(ref v) = req.payment_address_bittensor {
        match update_env_value("PAYMENT_ADDRESS_BITTENSOR", v) {
            Ok(_) => updated.push("PAYMENT_ADDRESS_BITTENSOR"),
            Err(e) => errors.push(format!("PAYMENT_ADDRESS_BITTENSOR: {}", e)),
        }
    }

    let port_fields: &[(&str, Option<u16>)] = &[
        ("SERVER_PUBLIC_PORT", req.server_public_port),
        ("SOCKS5_PORT", req.socks5_port),
        ("HTTP_PROXY_PORT", req.http_proxy_port),
        ("WIREGUARD_SERVERPORT", req.wireguard_server_port),
    ];
    for (env_key, value) in port_fields {
        if let Some(port) = value {
            if let Err(e) = validate_port(*port, env_key) {
                errors.push(e);
                continue;
            }
            match update_env_value(env_key, &port.to_string()) {
                Ok(_) => updated.push(env_key),
                Err(e) => errors.push(format!("{}: {}", env_key, e)),
            }
        }
    }

    if !errors.is_empty() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "success": false,
                "message": format!("部分更新失败: {}", errors.join("; ")),
                "updated": updated,
            })),
        );
    }

    if updated.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": false, "message": "未提供任何更新字段" })),
        );
    }

    (
        StatusCode::OK,
        Json(serde_json::json!({
            "success": true,
            "message": format!("已更新: {}，重启后生效", updated.join(", ")),
            "updated": updated,
        })),
    )
}
