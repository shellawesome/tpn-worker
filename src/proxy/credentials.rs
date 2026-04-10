use crate::db::socks5::{self, Socks5Config};
use crate::db::DbPool;
use crate::error::AppError;
use dashmap::DashMap;
use rand::Rng;
use std::path::Path;
use std::sync::Arc;
use subtle::ConstantTimeEq;
use tokio::fs;
use tracing::{info, warn};

/// In-memory credential manager for SOCKS5 and HTTP CONNECT proxy authentication.
/// Backed by the `worker_socks5_configs` SQLite table.
pub struct CredentialManager {
    /// username → password for O(1) auth lookup
    credentials: Arc<DashMap<String, String>>,
    pool: DbPool,
    host: String,
    port: u16,
    total_slots: usize,
    #[allow(dead_code)]
    priority_slots: i64,
}

impl CredentialManager {
    pub fn new(
        pool: DbPool,
        host: String,
        port: u16,
        total_slots: usize,
        priority_slots: i64,
    ) -> Self {
        Self {
            credentials: Arc::new(DashMap::new()),
            pool,
            host,
            port,
            total_slots,
            priority_slots,
        }
    }

    /// Initialize the credential store.
    /// Migration path: if /passwords/*.password files exist, import them.
    /// Otherwise load from DB. If DB is empty, generate fresh credentials.
    pub async fn initialize(&self, password_dir: &str) -> Result<(), AppError> {
        let password_path = Path::new(password_dir);

        // Check for legacy password files (migration from Dante)
        let has_password_files = if password_path.exists() {
            let mut entries = fs::read_dir(password_path).await.unwrap_or_else(|_| {
                // Return an empty iterator by trying a non-existent path
                panic!("Failed to read password directory");
            });
            let mut found = false;
            while let Ok(Some(entry)) = entries.next_entry().await {
                if entry.file_name().to_string_lossy().ends_with(".password")
                    && !entry
                        .file_name()
                        .to_string_lossy()
                        .ends_with(".password.used")
                {
                    found = true;
                    break;
                }
            }
            found
        } else {
            false
        };

        if has_password_files {
            info!("Found legacy password files, migrating from Dante...");
            self.migrate_from_password_files(password_dir).await?;
        } else {
            // Try loading from DB
            let count = self.load_from_db().await?;
            if count == 0 {
                info!(
                    "No credentials in DB, generating {} fresh credentials",
                    self.total_slots
                );
                self.generate_credentials().await?;
            } else {
                info!("Loaded {} credentials from database", count);
            }
        }

        // Sync host/port in DB to match current config (covers port changes via .env)
        self.sync_host_port().await?;

        Ok(())
    }

    /// Authenticate a username/password pair.
    /// Uses constant-time comparison to prevent timing side channels.
    pub fn authenticate(&self, username: &str, password: &str) -> bool {
        match self.credentials.get(username) {
            Some(stored) => {
                let stored_bytes = stored.as_bytes();
                let provided_bytes = password.as_bytes();
                // Constant-time equality check
                if stored_bytes.len() != provided_bytes.len() {
                    return false;
                }
                stored_bytes.ct_eq(provided_bytes).into()
            }
            None => false,
        }
    }

    /// Rotate the password for a given username.
    /// Updates both in-memory DashMap and database.
    pub async fn rotate_password(&self, username: &str) -> Result<String, AppError> {
        let new_password = generate_password();

        // Update in-memory
        if let Some(mut entry) = self.credentials.get_mut(username) {
            *entry = new_password.clone();
        } else {
            return Err(AppError::Internal(format!(
                "Cannot rotate password for unknown user: {}",
                username
            )));
        }

        // Update database
        let now_ms = chrono::Utc::now().timestamp_millis();
        sqlx::query(
            "UPDATE worker_socks5_configs SET password = $1, updated = $2 WHERE username = $3",
        )
        .bind(&new_password)
        .bind(now_ms)
        .bind(username)
        .execute(&self.pool)
        .await?;

        info!("Rotated password for {}", username);
        Ok(new_password)
    }

    /// Reload all credentials from database into DashMap.
    /// Called after cleanup to sync released leases.
    pub async fn reload_from_db(&self) -> Result<usize, AppError> {
        let count = self.load_from_db().await?;
        info!("Reloaded {} credentials from database", count);
        Ok(count)
    }

    /// Load credentials from DB into DashMap. Returns count loaded.
    async fn load_from_db(&self) -> Result<usize, AppError> {
        let configs: Vec<Socks5Config> = sqlx::query_as(
            "SELECT id, ip_address, port, username, password, available, expires_at, updated
             FROM worker_socks5_configs",
        )
        .fetch_all(&self.pool)
        .await?;

        self.credentials.clear();
        for config in &configs {
            self.credentials
                .insert(config.username.clone(), config.password.clone());
        }

        Ok(configs.len())
    }

    /// Generate fresh credentials and write to database.
    async fn generate_credentials(&self) -> Result<(), AppError> {
        let mut socks = Vec::with_capacity(self.total_slots);

        for _ in 0..self.total_slots {
            let username = generate_username();
            let password = generate_password();

            socks.push(Socks5Config {
                id: 0,
                ip_address: self.host.clone(),
                port: self.port as i32,
                username,
                password,
                available: true,
                expires_at: 0,
                updated: 0,
            });
        }

        socks5::write_socks(&self.pool, &socks).await?;

        // Load into memory
        for sock in &socks {
            self.credentials
                .insert(sock.username.clone(), sock.password.clone());
        }

        info!("Generated and stored {} credentials", self.total_slots);
        Ok(())
    }

    /// Migrate credentials from Dante password files.
    async fn migrate_from_password_files(&self, password_dir: &str) -> Result<(), AppError> {
        let password_path = Path::new(password_dir);
        let mut auth_files = Vec::new();
        let mut used_files = Vec::new();

        let mut entries = fs::read_dir(password_path).await?;
        while let Some(entry) = entries.next_entry().await? {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.ends_with(".password.used") {
                used_files.push(name);
            } else if name.ends_with(".password") {
                auth_files.push(name);
            }
        }

        info!(
            "Migrating {} credential files ({} used)",
            auth_files.len(),
            used_files.len()
        );

        let mut socks = Vec::new();
        for filename in &auth_files {
            let username = filename.trim_end_matches(".password").to_string();
            let available = !used_files.contains(&format!("{}.used", filename));
            let file_path = password_path.join(filename);
            let password = fs::read_to_string(&file_path)
                .await
                .unwrap_or_default()
                .trim()
                .to_string();

            if password.is_empty() {
                warn!("Skipping empty password file: {}", filename);
                continue;
            }

            socks.push(Socks5Config {
                id: 0,
                ip_address: self.host.clone(),
                port: self.port as i32,
                username,
                password,
                available,
                expires_at: 0,
                updated: 0,
            });
        }

        socks5::write_socks(&self.pool, &socks).await?;

        for sock in &socks {
            self.credentials
                .insert(sock.username.clone(), sock.password.clone());
        }

        info!("Migrated {} credentials from password files", socks.len());
        Ok(())
    }

    /// Update ip_address and port for all credentials in DB to match current config.
    async fn sync_host_port(&self) -> Result<(), AppError> {
        let result = sqlx::query(
            "UPDATE worker_socks5_configs SET ip_address = $1, port = $2
             WHERE ip_address != $1 OR port != $2",
        )
        .bind(&self.host)
        .bind(self.port as i32)
        .execute(&self.pool)
        .await?;

        let updated = result.rows_affected();
        if updated > 0 {
            info!(
                "Synced {} credentials to current host/port ({}:{})",
                updated, self.host, self.port
            );
        }
        Ok(())
    }

    pub fn credential_count(&self) -> usize {
        self.credentials.len()
    }
}

/// Generate a random username: u_{8 alphanumeric chars}
fn generate_username() -> String {
    let mut rng = rand::rng();
    let chars: String = (0..8)
        .map(|_| {
            let idx = rng.random_range(0..36u8);
            if idx < 10 {
                (b'0' + idx) as char
            } else {
                (b'a' + idx - 10) as char
            }
        })
        .collect();
    format!("u_{}", chars)
}

/// Generate a random password: p_{32 alphanumeric chars}
fn generate_password() -> String {
    let mut rng = rand::rng();
    let chars: String = (0..32)
        .map(|_| {
            let idx = rng.random_range(0..62u8);
            if idx < 10 {
                (b'0' + idx) as char
            } else if idx < 36 {
                (b'A' + idx - 10) as char
            } else {
                (b'a' + idx - 36) as char
            }
        })
        .collect();
    format!("p_{}", chars)
}
