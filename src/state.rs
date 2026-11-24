use std::sync::Arc;

use sqlx::SqlitePool;

use crate::config::Config;

/// Shared state for the HTTP server and its background tasks.
#[derive(Clone)]
pub struct AppState {
    pub pool: SqlitePool,
    pub config: Arc<Config>,
    /// Serializes billing so two registers cannot pick the same sale number.
    pub billing_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AppState {
    pub fn new(pool: SqlitePool, config: Config) -> Self {
        Self {
            pool,
            config: Arc::new(config),
            billing_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
}
