use clap::Parser;
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunMode {
    Validator,
    Miner,
    Worker,
}

impl fmt::Display for RunMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RunMode::Validator => write!(f, "validator"),
            RunMode::Miner => write!(f, "miner"),
            RunMode::Worker => write!(f, "worker"),
        }
    }
}

impl std::str::FromStr for RunMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "validator" => Ok(RunMode::Validator),
            "miner" => Ok(RunMode::Miner),
            "worker" => Ok(RunMode::Worker),
            _ => Err(format!(
                "Invalid RUN_MODE: '{}'. Must be one of: validator, miner, worker",
                s
            )),
        }
    }
}

#[derive(Debug, Clone, Parser)]
#[command(name = "tpn-worker", about = "TPN Subnet Worker Node")]
pub struct AppConfig {
    #[clap(env = "RUN_MODE", default_value = "worker")]
    pub run_mode: RunMode,

    // Server
    #[clap(env = "SERVER_PUBLIC_PORT", default_value = "3000")]
    pub server_port: u16,
    #[clap(env = "SERVER_PUBLIC_HOST", default_value = "")]
    pub server_public_host: String,
    #[clap(env = "SERVER_PUBLIC_PROTOCOL", default_value = "http")]
    pub server_public_protocol: String,
    #[clap(env = "SERVER_PUBLIC_URL", default_value = "")]
    pub server_public_url: String,

    // Mining pool
    #[clap(env = "MINING_POOL_URL", default_value = "http://157.90.237.67:3000")]
    pub mining_pool_url: String,
    #[clap(env = "MINING_POOL_NAME", default_value = "")]
    pub mining_pool_name: String,
    #[clap(env = "MINING_POOL_REWARDS", default_value = "")]
    pub mining_pool_rewards: String,
    #[clap(env = "MINING_POOL_WEBSITE_URL", default_value = "")]
    pub mining_pool_website_url: String,

    // WireGuard
    #[clap(env = "WIREGUARD_PEER_COUNT", default_value = "253")]
    pub wireguard_peer_count: u16,
    #[clap(env = "WIREGUARD_SERVERPORT", default_value = "51820")]
    pub wireguard_server_port: u16,
    #[clap(env = "WG_INTERFACE_NAME", default_value = "wg0")]
    pub wg_interface_name: String,
    #[clap(env = "WG_SUBNET", default_value = "10.13.13.0")]
    pub wg_subnet: String,
    #[clap(env = "WG_DNS", default_value = "1.1.1.1")]
    pub wg_dns: String,
    #[clap(env = "WG_PUBLIC_REACHABILITY_CHECK")]
    pub wg_public_reachability_check: bool,
    #[clap(env = "WG_PUBLIC_REACHABILITY_TIMEOUT_MS", default_value = "120000")]
    pub wg_public_reachability_timeout_ms: u64,

    // SOCKS5 / Proxy (Phase 2: embedded proxy replaces Dante + 3proxy)
    #[clap(env = "SOCKS5_PORT", default_value = "1080")]
    pub socks5_port: u16,
    #[clap(env = "HTTP_PROXY_PORT", default_value = "3128")]
    pub http_proxy_port: u16,
    #[clap(env = "PROXY_CREDENTIAL_COUNT", default_value = "256")]
    pub proxy_credential_count: usize,
    // Legacy (kept for backward compat / migration detection)
    #[clap(env = "DANTE_PORT", default_value = "1080")]
    pub dante_port: u16,
    #[clap(env = "PASSWORD_DIR", default_value = "/passwords")]
    pub password_dir: String,
    #[clap(
        env = "DANTE_REGEN_REQUEST_DIR",
        default_value = "/dante_regen_requests"
    )]
    pub dante_regen_dir: String,
    #[clap(env = "PRIORITY_SLOTS", default_value = "5")]
    pub priority_slots: i64,

    // Payment
    #[clap(
        env = "PAYMENT_ADDRESS_EVM",
        default_value = "0x1c89cb81123903Af1ecbbf3Edd688EfEDE119e12"
    )]
    pub payment_address_evm: String,
    #[clap(
        env = "PAYMENT_ADDRESS_BITTENSOR",
        default_value = "5EtYnZcY1UFbkwCp3HL5W3bxZCJshKtYVcTsWtB9QUtPTXxA"
    )]
    pub payment_address_bittensor: String,

    // Database (SQLite)
    // Runtime default is $HOME/.config/tpn-worker/tpn-worker.db (set in main.rs before parse)
    #[clap(
        env = "SQLITE_PATH",
        default_value = "~/.config/tpn-worker/tpn-worker.db"
    )]
    pub sqlite_path: String,

    // Feature flags
    #[clap(env = "CI_MODE")]
    pub ci_mode: bool,
    #[clap(env = "CI_MOCK_WG_CONTAINER")]
    pub ci_mock_wg_container: bool,
    #[clap(env = "CI_MOCK_WORKER_RESPONSES")]
    pub ci_mock_worker_responses: bool,
    #[clap(env = "CI_MOCK_MINING_POOL_RESPONSES")]
    pub ci_mock_mining_pool_responses: bool,
    #[clap(env = "FORCE_DESTROY_DATABASE")]
    pub force_destroy_database: bool,
    #[clap(env = "BETA_REFRESH_LEASE_INSTEAD_OF_DELETE")]
    pub beta_refresh_lease: bool,

    // Security
    #[clap(env = "LEASE_TOKEN_SECRET", default_value = "")]
    pub lease_token_secret: String,
    #[clap(env = "ADMIN_API_KEY", default_value = "")]
    pub admin_api_key: String,
    #[clap(env = "VALIDATOR_LEASE_API_KEYS", default_value = "")]
    pub validator_lease_api_keys: String,

    // Login / JWT
    #[clap(env = "LOGIN_PASSWORD", default_value = "")]
    pub login_password: String,
    #[clap(env = "JWT_SECRET", default_value = "")]
    pub jwt_secret: String,

    // Upgrade
    #[clap(env = "GITHUB_REPO", default_value = "shellawesome/tpn-worker")]
    pub github_repo: String,

    // Daemons
    #[clap(env = "DAEMON_INTERVAL_SECONDS", default_value = "300")]
    pub daemon_interval_s: u64,

    // Misc
    #[clap(env = "LOG_LEVEL", default_value = "info")]
    pub log_level: String,
    #[clap(env = "BROADCAST_MESSAGE", default_value = "")]
    pub broadcast_message: String,
    #[clap(env = "CONTACT_METHOD", default_value = "")]
    pub contact_method: String,

    // Network subnets for local request detection
    #[clap(env = "TPN_INTERNAL_SUBNET", default_value = "172.20.0.0/16")]
    pub tpn_internal_subnet: String,
    #[clap(env = "TPN_EXTERNAL_SUBNET", default_value = "172.21.0.0/16")]
    pub tpn_external_subnet: String,
}

impl AppConfig {
    /// Construct the base public URL for this node.
    pub fn base_url(&self) -> String {
        if !self.server_public_url.is_empty() {
            return self.server_public_url.trim_end_matches('/').to_string();
        }
        let host = &self.server_public_host;
        let port = self.server_port;
        let proto = &self.server_public_protocol;
        format!("{proto}://{host}:{port}")
    }

    /// Effective mining pool URL with trailing slash removed.
    pub fn effective_mining_pool_url(&self) -> String {
        let url = self.mining_pool_url.trim_end_matches('/');
        if url.is_empty() {
            // Fallback from JS code
            "http://165.227.133.192:3000".to_string()
        } else {
            url.to_string()
        }
    }

    /// Clamp wireguard_peer_count to [1, 253] (subnet /24 limit).
    pub fn effective_peer_count(&self) -> u16 {
        self.wireguard_peer_count.max(1).min(253)
    }

    pub fn is_worker(&self) -> bool {
        self.run_mode == RunMode::Worker
    }
}
