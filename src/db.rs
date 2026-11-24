//! The site's SQLite database (`DATABASE_URL`). The tables are in
//! `migrations/`, and opening the database brings them up to date.

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use std::str::FromStr;

/// The database in development, in the working directory; git ignores it.
pub const DEFAULT_URL: &str = "sqlite://taichichuan-mobi.db";

/// Opens the database at `url`, e.g. `sqlite:///var/lib/x/taichichuan-mobi.db`,
/// creating it if needed.
pub async fn open(url: &str) -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(10))
        .foreign_keys(true);
    migrate(SqlitePoolOptions::new().connect_with(options).await?).await
}

/// An empty database in memory, for tests.
#[cfg(test)]
pub async fn in_memory() -> Result<SqlitePool, sqlx::Error> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
    // One connection: each would have a database of its own.
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;
    migrate(pool).await
}

async fn migrate(pool: SqlitePool) -> Result<SqlitePool, sqlx::Error> {
    sqlx::migrate!().run(&pool).await?;
    Ok(pool)
}
