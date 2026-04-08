use crate::config::{AppConfig, RunMode};
use crate::db::DbPool;
use tracing::{info, warn};

/// Initialize database tables and indexes for worker mode.
pub async fn init_database(pool: &DbPool, config: &AppConfig) -> Result<(), sqlx::Error> {
    let worker_mode = config.run_mode == RunMode::Worker;

    // In CI/dev, optionally drop old tables
    if config.ci_mode || config.force_destroy_database {
        info!("Dropping old tables (CI/force mode)");
        for table in &[
            "workers",
            "worker_performance",
            "timestamps",
            "worker_broadcast_metadata",
            "mining_pool_metadata_broadcast",
            "challenge_solution",
            "scores",
            "worker_wireguard_configs",
            "worker_wg_server",
            "worker_socks5_configs",
            "worker_registration_log",
        ] {
            let sql = format!("DROP TABLE IF EXISTS {}", table);
            sqlx::query(&sql).execute(pool).await?;
        }
    }

    // Worker WireGuard configs table (full schema including Phase 3 columns)
    if worker_mode {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS worker_wireguard_configs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                expires_at INTEGER NOT NULL,
                updated_at TEXT NOT NULL DEFAULT '',
                private_key TEXT,
                public_key TEXT,
                preshared_key TEXT,
                allowed_ip TEXT
            )",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_wg_configs_expires_at
             ON worker_wireguard_configs (expires_at)",
        )
        .execute(pool)
        .await?;

        // WireGuard server key persistence table
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS worker_wg_server (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                private_key TEXT NOT NULL,
                public_key TEXT NOT NULL,
                created_at TEXT DEFAULT (datetime('now'))
            )",
        )
        .execute(pool)
        .await?;
        info!("Worker WireGuard tables initialized");
    }

    // Worker SOCKS5 configs table
    if worker_mode {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS worker_socks5_configs (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                ip_address TEXT NOT NULL,
                port INTEGER NOT NULL,
                username TEXT NOT NULL UNIQUE,
                password TEXT NOT NULL,
                available INTEGER NOT NULL DEFAULT 1,
                expires_at INTEGER NOT NULL,
                updated INTEGER NOT NULL
            )",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_socks5_available
             ON worker_socks5_configs (available, id)",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_socks5_expires_at
             ON worker_socks5_configs (expires_at)",
        )
        .execute(pool)
        .await?;

        // Unique constraint on username (with dedup on conflict)
        if let Err(e) = sqlx::query(
            "CREATE UNIQUE INDEX IF NOT EXISTS worker_socks5_configs_username_unique
             ON worker_socks5_configs (username)",
        )
        .execute(pool)
        .await
        {
            warn!(
                "Could not create unique index on username: {}, attempting dedup...",
                e
            );
            // Delete duplicates keeping the highest id
            let _ = sqlx::query(
                "DELETE FROM worker_socks5_configs WHERE id NOT IN (
                     SELECT MAX(id) FROM worker_socks5_configs GROUP BY username
                 )",
            )
            .execute(pool)
            .await;
            // Retry
            let _ = sqlx::query(
                "CREATE UNIQUE INDEX IF NOT EXISTS worker_socks5_configs_username_unique
                 ON worker_socks5_configs (username)",
            )
            .execute(pool)
            .await;
        }
        info!("Worker SOCKS5 configs table initialized");
    }

    // Timestamps table (all modes)
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS timestamps (
            label TEXT PRIMARY KEY,
            timestamp INTEGER NOT NULL,
            updated INTEGER NOT NULL
        )",
    )
    .execute(pool)
    .await?;
    info!("Timestamps table initialized");

    // Registration log table (worker mode)
    if worker_mode {
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS worker_registration_log (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                mining_pool_url TEXT NOT NULL,
                success INTEGER NOT NULL DEFAULT 0,
                status_code INTEGER NOT NULL DEFAULT 0,
                response_body TEXT NOT NULL DEFAULT '',
                error_message TEXT NOT NULL DEFAULT '',
                created_at TEXT NOT NULL
            )",
        )
        .execute(pool)
        .await?;

        sqlx::query(
            "CREATE INDEX IF NOT EXISTS idx_reg_log_created
             ON worker_registration_log (created_at)",
        )
        .execute(pool)
        .await?;
        info!("Registration log table initialized");
    }

    info!("Database initialization complete");
    Ok(())
}
