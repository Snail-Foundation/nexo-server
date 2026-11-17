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

        assert_eq!(table_count, 25);
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

        assert_eq!(applied, 5);
    }

    #[tokio::test]
    async fn data_and_sessions_survive_a_server_restart() {
        use axum::http::{HeaderMap, header::AUTHORIZATION};
        use axum_extra::extract::cookie::{Cookie, CookieJar};
        use nexo_core::{DeviceId, Operation, OperationKind, Uuid};

        use crate::auth::{self, SessionKind, SyncPrincipal};
        use crate::config::Config;
        use crate::snapshot;
        use crate::sync;

        let path = std::env::temp_dir().join(format!("nexo-server-restart-{}.db", Uuid::now_v7()));
        let url = format!("sqlite://{}?mode=rwc", path.display());

        let pool = connect(&url).await.unwrap();
        migrate(&pool).await.unwrap();

        let config = Config {
            database_url: url.clone(),
            addr: "127.0.0.1:0".to_owned(),
            admin_pin: Some("1234".to_owned()),
            business_name: "Café Central".to_owned(),
            session_hours: 12,
        };
        auth::seed_admin(&pool, &config).await.unwrap();

        let snapshot = snapshot::sample_snapshot();
        let item_id = snapshot.items[0].id.clone();
        let link = snapshot::link(
            &pool,
            snapshot::LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot,
            },
        )
        .await
        .unwrap();

        let operation = Operation::new(
            link.device_id.parse::<DeviceId>().unwrap(),
            Some(link.branch_id.parse().unwrap()),
            OperationKind::MenuItemAvailabilityChanged,
            item_id.parse::<Uuid>().unwrap(),
            serde_json::json!({ "is_available": false }),
            1_700_000_000_000,
        );
        sync::push(&pool, vec![operation]).await.unwrap();

        let login = auth::login(&pool, "Ana", "2468", SessionKind::Web, None, 12)
            .await
            .unwrap()
            .unwrap();
        let jar = CookieJar::new().add(Cookie::new(auth::SESSION_COOKIE, login.token));

        assert!(auth::web_session(&pool, &jar).await.unwrap().is_some());

        pool.close().await;

        // Reopen the same database as a restarted server would.
        let pool = connect(&url).await.unwrap();
        migrate(&pool).await.unwrap();

        let business_name: String =
            sqlx::query_scalar("SELECT name FROM business ORDER BY created_at_ms LIMIT 1")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(business_name, "Café Central");

        let available: bool =
            sqlx::query_scalar("SELECT is_available FROM menu_item WHERE id = ?1")
                .bind(&item_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(!available);

        let operations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operation_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(operations, 1);

        assert!(auth::web_session(&pool, &jar).await.unwrap().is_some());

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", link.device_token).parse().unwrap(),
        );
        assert!(matches!(
            auth::sync_principal(&pool, &headers).await.unwrap(),
            SyncPrincipal::Device(_)
        ));

        pool.close().await;

        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
        }
    }
}
