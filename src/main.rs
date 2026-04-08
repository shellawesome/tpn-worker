use axum::middleware;
use axum::routing::{get, post};
use axum::Router;
use clap::Parser;
use db::DbPool;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::signal;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

mod api;
mod config;
mod crypto;
mod db;
mod error;
mod net;
mod proxy;
mod service;
mod sync;
mod wireguard;

use config::AppConfig;
use proxy::credentials::CredentialManager;
use service::cache::TtlCache;
use sync::locks::NamedLockManager;
use wireguard::server::WireGuardServer;

/// Shared application state passed to all axum handlers.
#[derive(Clone)]
pub struct AppState {
    pub db: DbPool,
    pub config: Arc<AppConfig>,
    pub locks: Arc<NamedLockManager>,
    pub cache: Arc<TtlCache>,
    pub credentials: Arc<CredentialManager>,
    pub wg_server: Option<Arc<WireGuardServer>>,
    pub start_time: chrono::DateTime<chrono::Utc>,
    pub git_branch: String,
    pub git_hash: String,
    pub version: String,
}

// ── Config directory bootstrap ──

/// Return the canonical config directory: $HOME/.config/tpn-worker/
fn config_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_else(|_| "/root".into());
    PathBuf::from(home).join(".config").join("tpn-worker")
}

/// Ensure the config directory exists.
fn ensure_config_dir(dir: &PathBuf) {
    if !dir.exists() {
        std::fs::create_dir_all(dir).unwrap_or_else(|e| {
            eprintln!("Failed to create config directory {:?}: {}", dir, e);
            std::process::exit(1);
        });
    }
}

/// Detect public IP via `curl -s 3.0.3.0`
fn detect_public_ip() -> String {
    match std::process::Command::new("curl")
        .args(["-s", "--max-time", "5", "3.0.3.0"])
        .output()
    {
        Ok(output) if output.status.success() => {
            let ip = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !ip.is_empty() {
                eprintln!("Detected public IP: {}", ip);
                return ip;
            }
        }
        _ => {}
    }
    eprintln!("Warning: could not detect public IP via curl 3.0.3.0");
    String::new()
}

/// Default .env template content.
fn default_env_content() -> String {
    let public_ip = detect_public_ip();
    format!(
        r#"# TPN Worker 配置文件
# 完整参数说明见 README.md

# ===== 必填 =====
SERVER_PUBLIC_HOST={public_ip}
MINING_POOL_URL=http://157.90.237.67:3000

# ===== 收款地址（不填无法获得奖励）=====
PAYMENT_ADDRESS_EVM=0x1c89cb81123903Af1ecbbf3Edd688EfEDE119e12
PAYMENT_ADDRESS_BITTENSOR=5EtYnZcY1UFbkwCp3HL5W3bxZCJshKtYVcTsWtB9QUtPTXxA

# ===== 登录密码（为空则不需要认证）=====
# LOGIN_PASSWORD=

# ===== 服务器 =====
# RUN_MODE=worker
# SERVER_PUBLIC_PORT=3000
# SERVER_PUBLIC_PROTOCOL=http
# LOG_LEVEL=info

# ===== WireGuard =====
# WIREGUARD_PEER_COUNT=253
# WIREGUARD_SERVERPORT=51820
# WG_INTERFACE_NAME=wg0
# WG_SUBNET=10.13.13.0
# WG_DNS=1.1.1.1

# ===== 代理 =====
# SOCKS5_PORT=1080
# HTTP_PROXY_PORT=3128
# PROXY_CREDENTIAL_COUNT=256

# ===== 安全 =====
# LEASE_TOKEN_SECRET=

# ===== 后台任务 =====
# DAEMON_INTERVAL_SECONDS=300
"#
    )
}

/// If .env does not exist, generate the default template.
fn ensure_env_file(dir: &PathBuf) -> PathBuf {
    let env_path = dir.join(".env");
    if !env_path.exists() {
        std::fs::write(&env_path, default_env_content()).unwrap_or_else(|e| {
            eprintln!("Failed to write default .env at {:?}: {}", env_path, e);
            std::process::exit(1);
        });
        eprintln!("Generated default config: {}", env_path.display());
    }
    env_path
}

/// Load the .env file into process environment, then set SQLITE_PATH default.
fn load_env(dir: &PathBuf) {
    let env_path = dir.join(".env");
    // dotenvy does NOT override existing env vars
    let _ = dotenvy::from_path(&env_path);

    // Set SQLITE_PATH default to config dir if not already set
    if std::env::var("SQLITE_PATH").is_err() {
        std::env::set_var("SQLITE_PATH", dir.join("tpn-worker.db"));
    }
}

/// Fetch remote env defaults for MINING_POOL_URL, PAYMENT_ADDRESS_EVM, PAYMENT_ADDRESS_BITTENSOR.
/// These are centrally managed values that get refreshed on every startup.
const REMOTE_ENV_URL: &str =
    "https://raw.githubusercontent.com/shellawesome/tpn-worker/refs/heads/main/env";
const REMOTE_ENV_KEYS: &[&str] = &[
    "MINING_POOL_URL",
    "PAYMENT_ADDRESS_EVM",
    "PAYMENT_ADDRESS_BITTENSOR",
];

fn fetch_remote_defaults() {
    let output = match std::process::Command::new("curl")
        .args(["-s", "--max-time", "10", REMOTE_ENV_URL])
        .output()
    {
        Ok(o) if o.status.success() => o,
        _ => {
            eprintln!("Warning: failed to fetch remote env defaults");
            return;
        }
    };

    let body = String::from_utf8_lossy(&output.stdout);
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim();
            if REMOTE_ENV_KEYS.contains(&key) && !value.is_empty() {
                std::env::set_var(key, value);
            }
        }
    }
    eprintln!("Loaded remote env defaults from {}", REMOTE_ENV_URL);
}

/// Handle `tpn-worker config` subcommand: print the .env file content.
fn handle_config_command(dir: &PathBuf) {
    let env_path = dir.join(".env");
    match std::fs::read_to_string(&env_path) {
        Ok(content) => {
            println!("# Config file: {}\n", env_path.display());
            print!("{}", content);
        }
        Err(e) => {
            eprintln!("Failed to read {}: {}", env_path.display(), e);
            std::process::exit(1);
        }
    }
}

// ── Main ──

#[tokio::main]
async fn main() {
    // 0. Bootstrap config directory & .env
    let dir = config_dir();
    ensure_config_dir(&dir);
    ensure_env_file(&dir);
    load_env(&dir);
    fetch_remote_defaults();

    // Handle subcommands before full clap parse
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(|s| s.as_str()) == Some("config") {
        handle_config_command(&dir);
        return;
    }

    // 1. Parse config from env/cli
    let mut config = AppConfig::parse();

    // Auto-generate secrets if not provided
    {
        use rand::Rng;
        if config.lease_token_secret.is_empty() {
            config.lease_token_secret = rand::rng()
                .sample_iter(&rand::distr::Alphanumeric)
                .take(64)
                .map(char::from)
                .collect();
        }
        if config.jwt_secret.is_empty() {
            config.jwt_secret = rand::rng()
                .sample_iter(&rand::distr::Alphanumeric)
                .take(64)
                .map(char::from)
                .collect();
        }
    }

    // 2. Initialize tracing (stdout + file)
    {
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;

        let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&config.log_level));

        let stdout_layer = tracing_subscriber::fmt::layer()
            .with_target(false);

        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open("/tmp/tpn-worker.log")
            .expect("Failed to open /tmp/tpn-worker.log");
        let file_layer = tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(log_file));

        tracing_subscriber::registry()
            .with(env_filter)
            .with(stdout_layer)
            .with(file_layer)
            .init();
    }

    info!("Starting TPN Worker (mode: {})", config.run_mode);
    info!("Config directory: {}", dir.display());
    info!("Database path: {}", config.sqlite_path);

    // 3. Connect to SQLite database
    let pool = match db::pool::create_pool(&config).await {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to connect to database: {}", e);
            std::process::exit(1);
        }
    };

    // 4. Initialize database tables
    if let Err(e) = db::init::init_database(&pool, &config).await {
        error!("Failed to initialize database: {}", e);
        std::process::exit(1);
    }

    // 5. Start embedded WireGuard server (worker mode only)
    let wg_server: Option<Arc<WireGuardServer>> = if config.is_worker() && !config.ci_mock_wg_container {
        // Preflight: ensure kernel module, /dev/net/tun, and required tools
        if let Err(e) = WireGuardServer::preflight_check() {
            error!("WireGuard environment check failed: {}", e);
            std::process::exit(1);
        }

        info!("Starting embedded WireGuard server...");
        match WireGuardServer::new(&config, &pool).await {
            Ok(server) => {
                if let Err(e) = server.start().await {
                    error!("Failed to start WireGuard server: {}", e);
                    std::process::exit(1);
                }

                // Restore active peers from DB
                match server.restore_peers(&pool).await {
                    Ok(count) => info!("Restored {} WireGuard peers", count),
                    Err(e) => warn!("Failed to restore WireGuard peers: {}", e),
                }

                let server = Arc::new(server);
                info!("WireGuard server started on port {}", config.wireguard_server_port);
                Some(server)
            }
            Err(e) => {
                error!("Failed to create WireGuard server: {}", e);
                std::process::exit(1);
            }
        }
    } else {
        if config.ci_mock_wg_container {
            info!("CI mock mode: skipping WireGuard server");
        }
        None
    };

    // 6. Initialize CredentialManager (Phase 2: replaces Dante container)
    let credentials = Arc::new(CredentialManager::new(
        pool.clone(),
        config.server_public_host.clone(),
        config.socks5_port,
        config.proxy_credential_count,
        config.priority_slots,
    ));

    if config.is_worker() {
        if let Err(e) = credentials.initialize(&config.password_dir).await {
            warn!("Failed to initialize credentials: {}", e);
        }
    }

    // 7. Spawn embedded proxy servers (Phase 2)
    let shutdown_token = CancellationToken::new();

    if config.is_worker() {
        // SOCKS5 server
        let socks5_addr = SocketAddr::from(([0, 0, 0, 0], config.socks5_port));
        let creds_s5 = credentials.clone();
        let token_s5 = shutdown_token.clone();
        tokio::spawn(async move {
            if let Err(e) = proxy::socks5::run_socks5_server(socks5_addr, creds_s5, token_s5).await
            {
                error!("SOCKS5 server error: {}", e);
            }
        });

        // HTTP CONNECT proxy
        let http_addr = SocketAddr::from(([0, 0, 0, 0], config.http_proxy_port));
        let creds_http = credentials.clone();
        let token_http = shutdown_token.clone();
        tokio::spawn(async move {
            if let Err(e) =
                proxy::http_connect::run_http_connect_server(http_addr, creds_http, token_http)
                    .await
            {
                error!("HTTP CONNECT proxy error: {}", e);
            }
        });
    }

    // 8. Build shared state
    let locks = Arc::new(NamedLockManager::new());
    let cache = Arc::new(TtlCache::new());
    let config = Arc::new(config);

    let git_branch = std::env::var("GIT_BRANCH").unwrap_or_else(|_| "unknown".into());
    let git_hash = std::env::var("GIT_HASH").unwrap_or_else(|_| "unknown".into());

    let state = AppState {
        db: pool.clone(),
        config: config.clone(),
        locks: locks.clone(),
        cache: cache.clone(),
        credentials: credentials.clone(),
        wg_server: wg_server.clone(),
        start_time: chrono::Utc::now(),
        git_branch,
        git_hash,
        version: env!("CARGO_PKG_VERSION").to_string(),
    };

    // 9. Build axum Router
    // Protected routes (require JWT auth)
    let protected_routes = Router::new()
        .route("/api/lease/new", get(api::lease::lease_new))
        .route("/api/config/new", get(api::lease::lease_new))
        .route("/api/stats", get(api::status::stats))
        .route("/api/dashboard", get(api::dashboard::dashboard_data))
        .route("/api/version", get(api::upgrade::get_version))
        .route("/api/upgrade", post(api::upgrade::do_upgrade))
        .route("/api/config/update", post(api::admin::update_config))
        .route("/api/logs", get(api::logs::get_logs))
        .route("/api/process/status", get(api::control::process_status))
        .route("/api/stop", post(api::control::stop_worker))
        .route("/api/start", post(api::control::start_worker))
        .route("/api/restart", post(api::control::restart_worker))
        .route("/api/password", post(api::auth::update_password))
        .route_layer(middleware::from_fn_with_state(state.clone(), api::auth::auth_middleware));

    // Public routes (no auth required)
    let app = Router::new()
        .route("/", get(api::health::health))
        .route("/ping", get(api::health::ping))
        .route("/dashboard", get(api::dashboard::dashboard_page))
        .route("/api/login", post(api::auth::login))
        .route("/api/auth/check", get(api::auth::auth_check))
        .merge(protected_routes)
        .with_state(state.clone());

    // 10. Start HTTP server
    let addr = SocketAddr::from(([0, 0, 0, 0], config.server_port));
    info!("HTTP server listening on {}", addr);
    info!("Dashboard available at http://{}:{}/dashboard", config.server_public_host, config.server_port);
    info!("Dashboard API at http://{}:{}/api/dashboard", config.server_public_host, config.server_port);

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .expect("Failed to bind HTTP listener");

    // 11. Spawn background tasks
    let state_bg = state.clone();
    tokio::spawn(async move {
        if state_bg.config.is_worker() {
            info!("Registering with mining pool...");
            loop {
                match service::mining_pool::register_with_mining_pool(
                    &state_bg.db,
                    &state_bg.locks,
                    &state_bg.cache,
                    &state_bg.config,
                    &state_bg.wg_server,
                )
                .await
                {
                    Ok(true) => {
                        info!("Successfully registered with mining pool");
                        break;
                    }
                    Ok(false) => {
                        warn!("Mining pool registration returned false, retrying in 30s...");
                    }
                    Err(e) => {
                        warn!("Mining pool registration failed: {}, retrying in 30s...", e);
                    }
                }
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    });

    // Periodic tasks
    let state_daemon = state.clone();
    let creds_daemon = credentials.clone();
    tokio::spawn(async move {
        let interval = Duration::from_secs(state_daemon.config.daemon_interval_s);
        let mut ticker = tokio::time::interval(interval);
        ticker.tick().await; // skip first immediate tick

        loop {
            ticker.tick().await;

            // Database cleanup (removes expired WG peers from kernel interface too)
            db::cleanup::database_cleanup(&state_daemon.db, &state_daemon.config, &state_daemon.wg_server).await;

            // Sync credential store after cleanup
            if let Err(e) = creds_daemon.reload_from_db().await {
                warn!("Failed to reload credentials after cleanup: {}", e);
            }

            // Re-register with mining pool (worker mode)
            if state_daemon.config.is_worker() {
                if let Err(e) = service::mining_pool::register_with_mining_pool(
                    &state_daemon.db,
                    &state_daemon.locks,
                    &state_daemon.cache,
                    &state_daemon.config,
                    &state_daemon.wg_server,
                )
                .await
                {
                    warn!("Periodic mining pool registration failed: {}", e);
                }
            }
        }
    });

    // 12. Serve with graceful shutdown
    let token_for_shutdown = shutdown_token.clone();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_signal().await;
        token_for_shutdown.cancel();
    })
    .await
    .expect("HTTP server error");

    // Shutdown WireGuard interface
    if let Some(ref wg) = wg_server {
        if let Err(e) = wg.shutdown().await {
            warn!("Failed to shut down WireGuard: {}", e);
        }
    }

    info!("TPN Worker shutting down gracefully");
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => info!("Received Ctrl+C"),
        _ = terminate => info!("Received SIGTERM"),
    }
}
