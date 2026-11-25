use std::error::Error;

use nexo_server::auth;
use nexo_server::config::Config;
use nexo_server::db;
use nexo_server::router;
use nexo_server::state::AppState;
use tokio::net::TcpListener;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| EnvFilter::new("nexo_server=info,tower_http=info")),
        )
        .init();

    let config = Config::from_env()?;
    let pool = db::connect(&config.database_url).await?;
    db::migrate(&pool).await?;
    auth::seed_admin(&pool, &config).await?;

    let addr = config.addr.clone();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let printing_worker =
        tokio::spawn(nexo_server::printing::run_worker(pool.clone(), shutdown_rx));
    let state = AppState::new(pool, config);
    let app = router(state);
    let listener = TcpListener::bind(&addr).await?;

    tracing::info!("Nexo server listening on http://{}", listener.local_addr()?);

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    let _ = shutdown_tx.send(true);
    let _ = printing_worker.await;

    tracing::info!("Nexo server stopped");

    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {
            tracing::info!("shutdown requested by Ctrl+C");
        },
        () = terminate => {
            tracing::info!("shutdown requested by SIGTERM");
        },
    }
}
