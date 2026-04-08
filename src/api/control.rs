use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json};
use std::time::Duration;
use tracing::info;

use crate::AppState;

const BINARY: &str = "/usr/local/bin/tpn-worker";

/// GET /api/process/status — Check if tpn-worker is running, return PID & uptime
pub async fn process_status(State(state): State<AppState>) -> impl IntoResponse {
    let pid = std::process::id();
    let uptime = chrono::Utc::now()
        .signed_duration_since(state.start_time)
        .num_seconds();

    Json(serde_json::json!({
        "success": true,
        "data": {
            "running": true,
            "pid": pid,
            "uptime_seconds": uptime,
            "binary": BINARY,
            "start_time": state.start_time.to_rfc3339(),
            "version": format!("v{}", state.version),
        }
    }))
}

/// POST /api/stop — Gracefully stop the worker process
pub async fn stop_worker() -> impl IntoResponse {
    info!("Stop requested via API, shutting down in 500ms...");

    tokio::spawn(async {
        tokio::time::sleep(Duration::from_millis(500)).await;
        info!("Stopping tpn-worker...");
        std::process::exit(0);
    });

    Json(serde_json::json!({
        "success": true,
        "message": "正在停止服务..."
    }))
}

/// POST /api/start — Start a new tpn-worker process via nohup
pub async fn start_worker() -> impl IntoResponse {
    info!("Start requested via API, spawning new process...");

    match std::process::Command::new("sh")
        .args(["-c", &format!("nohup {} > /dev/null 2>&1 &", BINARY)])
        .spawn()
    {
        Ok(_) => {
            info!("New tpn-worker process spawned");
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "success": true,
                    "message": "新进程已启动"
                })),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "success": false,
                "message": format!("启动失败: {}", e)
            })),
        ),
    }
}

/// POST /api/restart — Stop current process, verify it's gone, start new one
pub async fn restart_worker() -> impl IntoResponse {
    let pid = std::process::id();
    info!("Restart requested via API (current PID: {})", pid);

    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(500)).await;

        // Spawn a detached shell script that:
        //   1. Waits for current process to die
        //   2. Verifies it's gone
        //   3. Starts a new instance
        let script = format!(
            "sleep 1; \
             while kill -0 {} 2>/dev/null; do sleep 0.2; done; \
             nohup {} > /dev/null 2>&1 &",
            pid, BINARY
        );

        info!("Spawning restart script, then exiting (PID {})...", pid);
        let _ = std::process::Command::new("sh")
            .args(["-c", &script])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();

        // Exit current process — the shell script will wait for us to die, then start new
        std::process::exit(0);
    });

    Json(serde_json::json!({
        "success": true,
        "message": "正在重启服务..."
    }))
}
