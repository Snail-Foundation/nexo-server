use std::str::FromStr;
use std::time::Duration;

use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};

use crate::error::AppError;

/// Opens the server database with foreign keys, WAL, and a busy timeout.
pub async fn connect(database_url: &str) -> Result<SqlitePool, AppError> {
    let options = SqliteConnectOptions::from_str(database_url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(5))
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal);

    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Opens a single-connection in-memory database, useful for tests.
pub async fn connect_in_memory() -> Result<SqlitePool, AppError> {
    let options = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);

    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await?;

    Ok(pool)
}

/// Applies the embedded SQLx migrations.
pub async fn migrate(pool: &SqlitePool) -> Result<(), AppError> {
    sqlx::migrate!("./migrations").run(pool).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn migrations_create_the_server_schema() {
        let pool = connect_in_memory().await.unwrap();
        migrate(&pool).await.unwrap();

        let table_count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*)
             FROM sqlite_master
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name NOT LIKE '\\_sqlx\\_migrations' ESCAPE '\\'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();

        assert_eq!(table_count, 10);
    }

    #[tokio::test]
    async fn migrations_are_idempotent() {
        let pool = connect_in_memory().await.unwrap();

        migrate(&pool).await.unwrap();
        migrate(&pool).await.unwrap();

        let applied: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(applied, 1);
    }
}
