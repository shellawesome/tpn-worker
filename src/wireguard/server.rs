use crate::config::AppConfig;
use crate::db::DbPool;
use crate::error::AppError;
use crate::wireguard::keygen;
use defguard_wireguard_rs::{
    key::Key, net::IpAddrMask, peer::Peer, InterfaceConfiguration, WGApi, WireguardInterfaceApi,
};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::path::Path;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{error, info, warn};

/// State for a single WireGuard peer on the server.
#[derive(Debug, Clone)]
pub struct PeerState {
    #[allow(dead_code)]
    pub peer_id: u16,
    pub private_key: String,
    pub public_key: String,
    pub preshared_key: String,
    pub allowed_ip: Ipv4Addr,
}

/// Client config returned after peer allocation.
#[derive(Debug, Clone)]
pub struct PeerConfig {
    pub peer_id: u16,
    pub client_private_key: String,
    pub client_address: String,
    pub server_public_key: String,
    pub preshared_key: String,
    pub endpoint: String,
    pub dns: String,
}

impl PeerConfig {
    /// Render as WireGuard INI text.
    /// ListenPort = 51820 is required by the mining pool's config parser.
    pub fn to_text(&self) -> String {
        format!(
            "[Interface]\n\
             Address = {}\n\
             PrivateKey = {}\n\
             ListenPort = 51820\n\
             DNS = {}\n\
             \n\
             [Peer]\n\
             PublicKey = {}\n\
             PresharedKey = {}\n\
             AllowedIPs = 0.0.0.0/0, ::/0\n\
             Endpoint = {}",
            self.client_address,
            self.client_private_key,
            self.dns,
            self.server_public_key,
            self.preshared_key,
            self.endpoint,
        )
    }

    /// Render as JSON value.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "interface": {
                "Address": self.client_address,
                "PrivateKey": self.client_private_key,
                "ListenPort": 51820,
                "DNS": self.dns,
            },
            "peer": {
                "PublicKey": self.server_public_key,
                "PresharedKey": self.preshared_key,
                "AllowedIPs": "0.0.0.0/0, ::/0",
                "Endpoint": self.endpoint,
            }
        })
    }
}

/// Embedded WireGuard server using defguard_wireguard_rs.
pub struct WireGuardServer {
    api: Mutex<WGApi<defguard_wireguard_rs::Kernel>>,
    interface_name: String,
    server_private_key: String,
    server_public_key: String,
    listen_port: u16,
    subnet_base: Ipv4Addr,
    endpoint_host: String,
    dns: String,
    peers: Arc<RwLock<HashMap<u16, PeerState>>>,
}

impl WireGuardServer {
    /// Check and prepare the system environment for WireGuard.
    /// Loads the kernel module if needed and ensures /dev/net/tun exists.
    pub fn preflight_check() -> Result<(), AppError> {
        // 1. Load WireGuard kernel module (no-op if already loaded or built-in)
        if !Path::new("/sys/module/wireguard").exists() {
            info!("WireGuard kernel module not loaded, attempting modprobe...");
            match std::process::Command::new("modprobe")
                .arg("wireguard")
                .output()
            {
                Ok(output) if output.status.success() => {
                    info!("WireGuard kernel module loaded successfully");
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    // Not fatal — kernel >= 5.6 has it built-in, /sys/module may not appear
                    warn!(
                        "modprobe wireguard: {} (may be built-in, continuing)",
                        stderr.trim()
                    );
                }
                Err(e) => {
                    warn!(
                        "Could not run modprobe: {} (may be built-in, continuing)",
                        e
                    );
                }
            }
        } else {
            info!("WireGuard kernel module is loaded");
        }

        // 2. Ensure /dev/net/tun exists
        if !Path::new("/dev/net/tun").exists() {
            info!("/dev/net/tun not found, creating...");
            let _ = std::fs::create_dir_all("/dev/net");
            match std::process::Command::new("mknod")
                .args(["/dev/net/tun", "c", "10", "200"])
                .output()
            {
                Ok(output) if output.status.success() => {
                    info!("Created /dev/net/tun");
                    // Set permissions
                    let _ = std::process::Command::new("chmod")
                        .args(["0666", "/dev/net/tun"])
                        .output();
                }
                Ok(output) => {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    return Err(AppError::Internal(format!(
                        "Failed to create /dev/net/tun: {}",
                        stderr.trim()
                    )));
                }
                Err(e) => {
                    return Err(AppError::Internal(format!(
                        "Failed to run mknod: {}. Install with: apt-get install -y coreutils",
                        e
                    )));
                }
            }
        } else {
            info!("/dev/net/tun exists");
        }

        // 3. Check that ip command is available (from iproute2, needed for routing setup)
        match std::process::Command::new("ip").arg("-V").output() {
            Ok(_) => {}
            Err(_) => {
                error!("`ip` command not found. Install with: apt-get install -y iproute2");
                return Err(AppError::Internal(
                    "`ip` command not found. Install iproute2: apt-get install -y iproute2".into(),
                ));
            }
        }

        Ok(())
    }

    /// Create the server, loading or generating server keys from the database.
    pub async fn new(config: &AppConfig, pool: &DbPool) -> Result<Self, AppError> {
        let interface_name = config.wg_interface_name.clone();
        let listen_port = config.wireguard_server_port;
        let subnet_base = config
            .wg_subnet
            .parse::<Ipv4Addr>()
            .map_err(|e| AppError::Internal(format!("Invalid WG subnet: {}", e)))?;
        let endpoint_host = config.server_public_host.clone();
        let dns = config.wg_dns.clone();

        // Load or generate server keypair
        let (server_private_key, server_public_key) = load_or_create_server_keys(pool).await?;

        let api = WGApi::<defguard_wireguard_rs::Kernel>::new(interface_name.clone())
            .map_err(|e| AppError::Internal(format!("Failed to create WGApi: {}", e)))?;

        Ok(Self {
            api: Mutex::new(api),
            interface_name,
            server_private_key,
            server_public_key,
            listen_port,
            subnet_base,
            endpoint_host,
            dns,
            peers: Arc::new(RwLock::new(HashMap::new())),
        })
    }

    /// Create the WireGuard interface and configure it.
    pub async fn start(&self) -> Result<(), AppError> {
        let mut api = self.api.lock().await;

        // Create the network interface
        api.create_interface()
            .map_err(|e| AppError::Internal(format!("Failed to create WG interface: {}", e)))?;

        // Server address: subnet_base + 1 (e.g., 10.13.13.1)
        let server_ip = self.server_address();

        let interface_config = InterfaceConfiguration {
            name: self.interface_name.clone(),
            prvkey: self.server_private_key.clone(),
            addresses: vec![server_ip.parse().map_err(|e| {
                AppError::Internal(format!("Invalid server IP '{}': {}", server_ip, e))
            })?],
            port: self.listen_port,
            peers: vec![],
            mtu: None,
            fwmark: None,
        };

        api.configure_interface(&interface_config)
            .map_err(|e| AppError::Internal(format!("Failed to configure WG interface: {}", e)))?;

        info!(
            "WireGuard interface {} started on port {} (address {})",
            self.interface_name, self.listen_port, server_ip
        );

        // Enable IP forwarding and NAT for VPN traffic
        self.setup_networking().await;

        Ok(())
    }

    /// Configure IP forwarding and iptables NAT rules for VPN traffic.
    async fn setup_networking(&self) {
        // Enable IP forwarding
        match std::fs::write("/proc/sys/net/ipv4/ip_forward", "1") {
            Ok(_) => info!("Enabled IPv4 forwarding"),
            Err(e) => error!(
                "Failed to enable IPv4 forwarding: {} (run as root or set manually)",
                e
            ),
        }

        // Detect default outbound interface
        let outbound_iface = detect_default_interface().unwrap_or_else(|| "eth0".to_string());
        let subnet = format!("{}/24", self.subnet_base);

        // Add MASQUERADE rule for WireGuard subnet
        let status = std::process::Command::new("iptables")
            .args([
                "-t",
                "nat",
                "-C",
                "POSTROUTING",
                "-s",
                &subnet,
                "-o",
                &outbound_iface,
                "-j",
                "MASQUERADE",
            ])
            .output();

        let rule_exists = status.map(|o| o.status.success()).unwrap_or(false);

        if !rule_exists {
            match std::process::Command::new("iptables")
                .args([
                    "-t",
                    "nat",
                    "-A",
                    "POSTROUTING",
                    "-s",
                    &subnet,
                    "-o",
                    &outbound_iface,
                    "-j",
                    "MASQUERADE",
                ])
                .output()
            {
                Ok(output) if output.status.success() => {
                    info!(
                        "Added NAT MASQUERADE rule for {} via {}",
                        subnet, outbound_iface
                    );
                }
                Ok(output) => {
                    error!(
                        "Failed to add NAT rule: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Err(e) => {
                    error!(
                        "Failed to run iptables: {} (ensure iptables is installed)",
                        e
                    );
                }
            }
        } else {
            info!("NAT MASQUERADE rule already exists for {}", subnet);
        }

        // Add route for WireGuard subnet via the WG interface
        // Without this, return traffic for 10.13.13.x has no route to wg0
        let route_exists = std::process::Command::new("ip")
            .args(["route", "show", &subnet])
            .output()
            .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
            .unwrap_or(false);

        if !route_exists {
            match std::process::Command::new("ip")
                .args(["route", "add", &subnet, "dev", &self.interface_name])
                .output()
            {
                Ok(output) if output.status.success() => {
                    info!("Added route {} dev {}", subnet, self.interface_name);
                }
                Ok(output) => {
                    error!(
                        "Failed to add route: {}",
                        String::from_utf8_lossy(&output.stderr)
                    );
                }
                Err(e) => {
                    error!("Failed to run ip route: {}", e);
                }
            }
        } else {
            info!("Route for {} already exists", subnet);
        }

        // Allow forwarding for WireGuard interface
        let wg_iface = &self.interface_name;
        let _ = std::process::Command::new("iptables")
            .args(["-C", "FORWARD", "-i", wg_iface, "-j", "ACCEPT"])
            .output()
            .and_then(|o| {
                if !o.status.success() {
                    std::process::Command::new("iptables")
                        .args(["-A", "FORWARD", "-i", wg_iface, "-j", "ACCEPT"])
                        .output()
                } else {
                    Ok(o)
                }
            });

        let _ = std::process::Command::new("iptables")
            .args([
                "-C",
                "FORWARD",
                "-o",
                wg_iface,
                "-m",
                "state",
                "--state",
                "ESTABLISHED,RELATED",
                "-j",
                "ACCEPT",
            ])
            .output()
            .and_then(|o| {
                if !o.status.success() {
                    std::process::Command::new("iptables")
                        .args([
                            "-A",
                            "FORWARD",
                            "-o",
                            wg_iface,
                            "-m",
                            "state",
                            "--state",
                            "ESTABLISHED,RELATED",
                            "-j",
                            "ACCEPT",
                        ])
                        .output()
                } else {
                    Ok(o)
                }
            });

        // Accept inbound WireGuard handshake traffic on the public UDP port.
        let port = self.listen_port.to_string();
        let _ = std::process::Command::new("iptables")
            .args(["-C", "INPUT", "-p", "udp", "--dport", &port, "-j", "ACCEPT"])
            .output()
            .and_then(|o| {
                if !o.status.success() {
                    std::process::Command::new("iptables")
                        .args(["-A", "INPUT", "-p", "udp", "--dport", &port, "-j", "ACCEPT"])
                        .output()
                } else {
                    Ok(o)
                }
            });
    }

    /// Best-effort local check that the public WireGuard UDP port is reachable.
    pub fn wait_for_public_udp_reachability(&self, max_wait_ms: u64) -> bool {
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < max_wait_ms as u128 {
            if self.check_public_udp_reachability() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
        false
    }

    /// Single-shot public UDP reachability probe for the configured WireGuard port.
    pub fn public_udp_reachable(&self) -> bool {
        self.check_public_udp_reachability()
    }

    fn check_public_udp_reachability(&self) -> bool {
        if self.endpoint_host.trim().is_empty() {
            warn!("Skipping WireGuard reachability check: SERVER_PUBLIC_HOST is empty");
            return false;
        }

        let output = std::process::Command::new("nc")
            .args([
                "-vzu",
                "-w",
                "10",
                self.endpoint_host.as_str(),
                self.listen_port.to_string().as_str(),
            ])
            .output();

        match output {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let combined = format!("{stdout} {stderr}");
                let reachable = combined.contains("succeeded");
                info!(
                    "WireGuard public UDP reachability check host={} port={} reachable={}",
                    self.endpoint_host, self.listen_port, reachable
                );
                reachable
            }
            Err(e) => {
                warn!("WireGuard reachability check failed to run nc: {}", e);
                false
            }
        }
    }

    /// Restore active peers from the database on restart.
    pub async fn restore_peers(&self, pool: &DbPool) -> Result<usize, AppError> {
        let rows = sqlx::query_as::<_, PeerRow>(
            "SELECT id, private_key, public_key, preshared_key, allowed_ip
             FROM worker_wireguard_configs
             WHERE expires_at > $1
               AND private_key IS NOT NULL
               AND public_key IS NOT NULL",
        )
        .bind(chrono::Utc::now().timestamp_millis())
        .fetch_all(pool)
        .await?;

        let mut count = 0;
        let mut peers = self.peers.write().await;
        for row in &rows {
            let peer_id = row.id as u16;
            let pub_key = match row.public_key.as_ref() {
                Some(k) => k.clone(),
                None => continue,
            };
            let psk = row.preshared_key.clone().unwrap_or_default();
            let allowed_ip_str = row
                .allowed_ip
                .clone()
                .unwrap_or_else(|| self.peer_ip(peer_id).to_string());

            // Add peer to the WireGuard interface
            if let Err(e) = self
                .add_peer_to_interface(&pub_key, &psk, &allowed_ip_str)
                .await
            {
                warn!("Failed to restore peer {}: {}", peer_id, e);
                continue;
            }

            peers.insert(
                peer_id,
                PeerState {
                    peer_id,
                    private_key: row.private_key.clone().unwrap_or_default(),
                    public_key: pub_key,
                    preshared_key: psk,
                    allowed_ip: allowed_ip_str.parse().unwrap_or(self.peer_ip(peer_id)),
                },
            );
            count += 1;
        }

        info!("Restored {} active WireGuard peers", count);
        Ok(count)
    }

    /// Allocate and add a new peer to the WireGuard interface.
    pub async fn add_peer(&self, peer_id: u16) -> Result<PeerConfig, AppError> {
        let keypair = keygen::generate_keypair();
        let psk = keygen::generate_preshared_key();
        let allowed_ip = self.peer_ip(peer_id);
        let allowed_ip_str = format!("{}/32", allowed_ip);

        // Add to WireGuard interface
        self.add_peer_to_interface(&keypair.public_key, &psk, &allowed_ip_str)
            .await?;

        // Store in memory
        let state = PeerState {
            peer_id,
            private_key: keypair.private_key.clone(),
            public_key: keypair.public_key.clone(),
            preshared_key: psk.clone(),
            allowed_ip,
        };
        self.peers.write().await.insert(peer_id, state);

        Ok(PeerConfig {
            peer_id,
            client_private_key: keypair.private_key,
            client_address: allowed_ip.to_string(),
            server_public_key: self.server_public_key.clone(),
            preshared_key: psk,
            endpoint: format!("{}:{}", self.endpoint_host, self.listen_port),
            dns: self.dns.clone(),
        })
    }

    /// Remove a peer from the WireGuard interface.
    pub async fn remove_peer(&self, peer_id: u16) -> Result<(), AppError> {
        // Atomically remove from map first to prevent races
        let state = self.peers.write().await.remove(&peer_id);

        if let Some(state) = state {
            let key_bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                &state.public_key,
            )
            .map_err(|e| AppError::Internal(format!("Invalid public key: {}", e)))?;
            let key: Key = key_bytes
                .as_slice()
                .try_into()
                .map_err(|e| AppError::Internal(format!("Key conversion failed: {}", e)))?;

            let api = self.api.lock().await;
            api.remove_peer(&key).map_err(|e| {
                AppError::Internal(format!("Failed to remove peer {}: {}", peer_id, e))
            })?;
        }

        info!("Removed WireGuard peer {}", peer_id);
        Ok(())
    }

    /// Rotate keys for an existing peer (remove old, add new).
    pub async fn rotate_peer_keys(&self, peer_id: u16) -> Result<PeerConfig, AppError> {
        // Remove old peer from interface (ignore error if not present)
        let _ = self.remove_peer(peer_id).await;
        // Add new peer with fresh keys
        self.add_peer(peer_id).await
    }

    /// Get the client configuration for an active peer.
    pub async fn get_client_config(&self, peer_id: u16) -> Result<PeerConfig, AppError> {
        let peers = self.peers.read().await;
        let state = peers.get(&peer_id).ok_or_else(|| {
            AppError::Internal(format!("Peer {} not found in active peers", peer_id))
        })?;

        Ok(PeerConfig {
            peer_id,
            client_private_key: state.private_key.clone(),
            client_address: state.allowed_ip.to_string(),
            server_public_key: self.server_public_key.clone(),
            preshared_key: state.preshared_key.clone(),
            endpoint: format!("{}:{}", self.endpoint_host, self.listen_port),
            dns: self.dns.clone(),
        })
    }

    /// Shut down the WireGuard interface.
    pub async fn shutdown(&self) -> Result<(), AppError> {
        let api = self.api.lock().await;
        api.remove_interface()
            .map_err(|e| AppError::Internal(format!("Failed to remove WG interface: {}", e)))?;
        info!("WireGuard interface {} removed", self.interface_name);
        Ok(())
    }

    /// Return the number of currently active WireGuard peers.
    pub async fn active_peer_count(&self) -> usize {
        self.peers.read().await.len()
    }

    /// Return the server public key.
    pub fn server_public_key(&self) -> &str {
        &self.server_public_key
    }

    /// Return the interface name.
    pub fn interface_name(&self) -> &str {
        &self.interface_name
    }

    // ── Internal helpers ──

    fn server_address(&self) -> String {
        let octets = self.subnet_base.octets();
        format!(
            "{}.{}.{}.{}",
            octets[0],
            octets[1],
            octets[2],
            octets[3] + 1
        )
    }

    fn peer_ip(&self, peer_id: u16) -> Ipv4Addr {
        let octets = self.subnet_base.octets();
        // peer_id is clamped to 1..253 by effective_peer_count(), so last_octet <= 254
        let last_octet = (octets[3] as u16) + peer_id + 1;
        debug_assert!(last_octet <= 255, "Peer IP overflow: {}", last_octet);
        Ipv4Addr::new(octets[0], octets[1], octets[2], last_octet as u8)
    }

    async fn add_peer_to_interface(
        &self,
        public_key_b64: &str,
        preshared_key_b64: &str,
        allowed_ip: &str,
    ) -> Result<(), AppError> {
        let key_bytes =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, public_key_b64)
                .map_err(|e| AppError::Internal(format!("Invalid public key base64: {}", e)))?;
        let key: Key = key_bytes
            .as_slice()
            .try_into()
            .map_err(|e| AppError::Internal(format!("Key conversion error: {}", e)))?;

        let mut peer = Peer::new(key);

        // Set preshared key
        if !preshared_key_b64.is_empty() {
            let psk_bytes = base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                preshared_key_b64,
            )
            .map_err(|e| AppError::Internal(format!("Invalid PSK base64: {}", e)))?;
            let psk_key: Key = psk_bytes
                .as_slice()
                .try_into()
                .map_err(|e| AppError::Internal(format!("PSK key conversion error: {}", e)))?;
            peer.preshared_key = Some(psk_key);
        }

        // Set allowed IPs
        let addr = IpAddrMask::from_str(allowed_ip).map_err(|e| {
            AppError::Internal(format!("Invalid allowed IP '{}': {}", allowed_ip, e))
        })?;
        peer.allowed_ips.push(addr);

        // Set persistent keepalive (25 seconds, standard for NAT traversal)
        peer.persistent_keepalive_interval = Some(25);

        let api = self.api.lock().await;
        api.configure_peer(&peer)
            .map_err(|e| AppError::Internal(format!("Failed to configure peer: {}", e)))?;

        Ok(())
    }
}

// ── Network helpers ──

/// Detect the default outbound network interface from the routing table.
fn detect_default_interface() -> Option<String> {
    let output = std::process::Command::new("ip")
        .args(["route", "show", "default"])
        .output()
        .ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Format: "default via X.X.X.X dev eth0 ..."
    for line in stdout.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if let Some(pos) = parts.iter().position(|&p| p == "dev") {
            if let Some(iface) = parts.get(pos + 1) {
                info!("Detected default outbound interface: {}", iface);
                return Some(iface.to_string());
            }
        }
    }
    None
}

// ── DB helpers for server key persistence ──

#[derive(sqlx::FromRow)]
struct ServerKeyRow {
    private_key: String,
    public_key: String,
}

#[derive(sqlx::FromRow)]
struct PeerRow {
    id: i32,
    private_key: Option<String>,
    public_key: Option<String>,
    preshared_key: Option<String>,
    allowed_ip: Option<String>,
}

/// Load existing server keys from DB, or generate and store new ones.
async fn load_or_create_server_keys(pool: &DbPool) -> Result<(String, String), AppError> {
    let existing: Option<ServerKeyRow> = sqlx::query_as(
        "SELECT private_key, public_key FROM worker_wg_server ORDER BY id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;

    if let Some(row) = existing {
        info!("Loaded existing WireGuard server keys from database");
        return Ok((row.private_key, row.public_key));
    }

    // Generate new server keypair
    let keypair = keygen::generate_keypair();
    sqlx::query("INSERT INTO worker_wg_server (private_key, public_key) VALUES ($1, $2)")
        .bind(&keypair.private_key)
        .bind(&keypair.public_key)
        .execute(pool)
        .await?;

    info!("Generated and stored new WireGuard server keys");
    Ok((keypair.private_key, keypair.public_key))
}
