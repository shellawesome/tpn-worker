use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use serde::{Deserialize, Serialize};
use std::time::Duration;
use tracing::{error, info};

use crate::AppState;

#[derive(Serialize)]
struct VersionInfo {
    current: String,
    latest: Option<String>,
    has_update: bool,
    download_url: Option<String>,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// GET /api/version — Check current version and latest GitHub release
pub async fn get_version(State(state): State<AppState>) -> impl IntoResponse {
    let current = format!("v{}", state.version);
    let repo = &state.config.github_repo;

    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => {
            return Json(serde_json::json!({
                "success": true,
                "data": VersionInfo { current, latest: None, has_update: false, download_url: None }
            }));
        }
    };

    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);
    let resp = client
        .get(&url)
        .header("User-Agent", "tpn-worker")
        .send()
        .await;

    match resp {
        Ok(r) => {
            if let Ok(release) = r.json::<GitHubRelease>().await {
                let latest = release.tag_name.clone();
                let latest_clean = latest.trim_start_matches('v');
                let current_clean = current.trim_start_matches('v');
                let has_update = latest_clean != current_clean;

                let asset_name = "tpn-worker-linux-ubuntu22-amd64";

                let download_url = release
                    .assets
                    .iter()
                    .find(|a| a.name == asset_name)
                    .map(|a| a.browser_download_url.clone());

                Json(serde_json::json!({
                    "success": true,
                    "data": VersionInfo { current, latest: Some(latest), has_update, download_url }
                }))
            } else {
                Json(serde_json::json!({
                    "success": true,
                    "data": VersionInfo { current, latest: None, has_update: false, download_url: None }
                }))
            }
        }
        Err(_) => Json(serde_json::json!({
            "success": true,
            "data": VersionInfo { current, latest: None, has_update: false, download_url: None }
        })),
    }
}

/// POST /api/upgrade — Download latest release and replace binary
pub async fn do_upgrade(State(state): State<AppState>) -> impl IntoResponse {
    let repo = &state.config.github_repo;

    // 1. Determine download URL
    let download_url = format!(
        "https://github.com/{}/releases/download/latest/tpn-worker-linux-ubuntu22-amd64",
        repo
    );

    info!("Downloading update from: {}", download_url);

    // 2. Download
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "success": false, "message": format!("创建下载客户端失败: {}", e) })),
            );
        }
    };

    let binary_data = match client
        .get(&download_url)
        .header("User-Agent", "tpn-worker")
        .send()
        .await
    {
        Ok(r) => {
            if !r.status().is_success() {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({ "success": false, "message": format!("下载失败, HTTP {}", r.status()) })),
                );
            }
            match r.bytes().await {
                Ok(b) => b,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "success": false, "message": format!("读取下载数据失败: {}", e) })),
                    );
                }
            }
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "success": false, "message": format!("下载失败: {}", e) })),
            );
        }
    };

    let temp_path = "/tmp/tpn-worker-new";

    // 3. Write temp file
    if let Err(e) = std::fs::write(temp_path, &binary_data) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": format!("写入临时文件失败: {}", e) })),
        );
    }

    // 4. Make executable
    use std::os::unix::fs::PermissionsExt;
    if let Err(e) = std::fs::set_permissions(temp_path, std::fs::Permissions::from_mode(0o755)) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": format!("设置权限失败: {}", e) })),
        );
    }

    // 5. Verify new binary
    let verify = tokio::process::Command::new(temp_path)
        .arg("--help")
        .output()
        .await;
    if verify.is_err() {
        let _ = std::fs::remove_file(temp_path);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": "新二进制验证失败" })),
        );
    }

    // 6. Get current exe path
    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({ "success": false, "message": format!("获取当前二进制路径失败: {}", e) })),
            );
        }
    };

    // 7. Backup current binary
    let backup_path = format!("{}.bak", current_exe.display());
    if let Err(e) = std::fs::copy(&current_exe, &backup_path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": format!("备份失败: {}", e) })),
        );
    }

    // 8. Replace: delete then copy (Linux allows deleting running executables)
    if let Err(e) = std::fs::remove_file(&current_exe) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": format!("删除旧二进制失败: {}", e) })),
        );
    }
    if let Err(e) = std::fs::copy(temp_path, &current_exe) {
        let _ = std::fs::copy(&backup_path, &current_exe);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": format!("替换二进制失败: {}", e) })),
        );
    }
    if let Err(e) = std::fs::set_permissions(&current_exe, std::fs::Permissions::from_mode(0o755))
    {
        let _ = std::fs::remove_file(&current_exe);
        let _ = std::fs::copy(&backup_path, &current_exe);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": format!("设置新二进制权限失败: {}", e) })),
        );
    }
    let _ = std::fs::remove_file(temp_path);

    info!("Upgrade successful! Scheduling restart...");

    // 9. Restart via systemd or exec
    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Try systemd restart first
        let systemd_result = tokio::process::Command::new("systemctl")
            .args(["restart", "tpn-worker"])
            .output()
            .await;

        if let Ok(output) = systemd_result {
            if output.status.success() {
                return;
            }
        }

        // Fallback: exit and let process manager restart
        error!("Systemd restart failed, exiting for process manager to restart");
        std::process::exit(0);
    });

    (
        StatusCode::OK,
        Json(serde_json::json!({ "success": true, "message": "更新成功，正在重启..." })),
    )
}
