//! Nexo local server: synchronization hub, auth, and the waiter/kitchen web app.

pub mod api;
pub mod auth;
pub mod config;
pub mod db;
pub mod error;
pub mod projection;
pub mod snapshot;
pub mod state;
pub mod sync;
pub mod time;
pub mod web;

use axum::Router;
use axum::routing::{get, post};
use tower_http::trace::TraceLayer;

use crate::state::AppState;

/// Builds the HTTP router. Shared by the binary and the tests.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(web::root))
        .route("/login", get(web::login_page).post(web::login_submit))
        .route("/logout", post(web::logout))
        .route("/app", get(web::app_page))
        .route("/app/tables", get(web::tables_page))
        .route("/app/tables/{id}/open", post(web::open_table))
        .route(
            "/app/takeout",
            get(web::takeout_page).post(web::open_takeout),
        )
        .route("/app/orders/{id}", get(web::order_page))
        .route("/app/orders/{id}/lines", post(web::add_order_line))
        .route("/app/orders/{id}/send", post(web::send_round))
        .route("/app/orders/{id}/ready", post(web::mark_order_ready))
        .route("/app/orders/{id}/served", post(web::mark_order_served))
        .route(
            "/app/orders/{id}/lines/{line_id}/void",
            post(web::void_line),
        )
        .route(
            "/app/orders/{id}/lines/{line_id}/ready",
            post(web::mark_line_ready),
        )
        .route(
            "/app/orders/{id}/lines/{line_id}/served",
            post(web::mark_line_served),
        )
        .route("/app/kitchen", get(web::kitchen_page))
        .route("/app/kitchen/board", get(web::kitchen_board))
        .route("/app/menu", get(web::menu_page))
        .route(
            "/app/menu/items/{id}/availability",
            post(web::toggle_item_availability),
        )
        .route("/app/status", get(web::app_status))
        .route("/static/app.css", get(web::stylesheet))
        .route("/static/htmx.min.js", get(web::htmx))
        .route("/api/health", get(api::health))
        .route("/api/login", post(api::login))
        .route("/api/link", post(api::link))
        .route("/api/sync/push", post(api::push))
        .route("/api/sync/pull", get(api::pull))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

#[cfg(test)]
pub(crate) mod test_support {
    use crate::auth;
    use crate::config::Config;
    use crate::db;
    use crate::state::AppState;

    pub(crate) async fn state() -> AppState {
        let pool = db::connect_in_memory().await.unwrap();
        db::migrate(&pool).await.unwrap();

        let config = Config {
            database_url: "sqlite::memory:".to_owned(),
            addr: "127.0.0.1:0".to_owned(),
            admin_pin: Some("1234".to_owned()),
            business_name: "Café Central".to_owned(),
            session_hours: 12,
        };

        auth::seed_admin(&pool, &config).await.unwrap();

        AppState::new(pool, config)
    }
}
