use crate::config::AppConfig;
use crate::db::DbPool;
use sqlx::sqlite::SqlitePoolOptions;
use tracing::info;

/// Create a SQLite connection pool with WAL mode.
pub async fn create_pool(config: &AppConfig) -> Result<DbPool, sqlx::Error> {
    let url = format!("sqlite:{}?mode=rwc", config.sqlite_path);
    info!("Opening SQLite database at {}", config.sqlite_path);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await?;

    // Enable WAL mode for concurrent reads
    sqlx::query("PRAGMA journal_mode=WAL")
        .execute(&pool)
        .await?;
    // Set busy timeout so writers wait instead of failing immediately
    sqlx::query("PRAGMA busy_timeout=5000")
        .execute(&pool)
        .await?;
    sqlx::query("PRAGMA foreign_keys=ON")
        .execute(&pool)
        .await?;

    info!("SQLite database ready (WAL mode)");
    Ok(pool)
}
