use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use nexo_core::{BranchId, DeviceId, Operation, OperationKind, Uuid};
use serde::Deserialize;
use serde_json::json;

use crate::auth::{self, SessionKind, SessionUser};
use crate::error::AppError;
use crate::projection;
use crate::snapshot;
use crate::state::AppState;
use crate::sync;
use crate::time::{format_local_time, now_ms};

const STYLESHEET: &str = include_str!("../static/app.css");
const HTMX: &str = include_str!("../static/htmx.min.js");

#[derive(Clone)]
struct PageContext {
    business_name: String,
    user_name: String,
    role_label: String,
    csrf_token: String,
    version: &'static str,
}

fn page_context(session: &SessionUser) -> PageContext {
    PageContext {
        business_name: session.business_name.clone(),
        user_name: session.display_name.clone(),
        role_label: auth::role_label(&session.role).to_owned(),
        csrf_token: session.csrf_token.clone(),
        version: env!("CARGO_PKG_VERSION"),
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: String,
}

#[derive(Template)]
#[template(path = "app.html")]
struct AppTemplate {
    common: PageContext,
    active: &'static str,
}

#[derive(Template)]
#[template(path = "tables.html")]
struct TablesTemplate {
    common: PageContext,
    active: &'static str,
    tables: Vec<TableView>,
}

#[derive(Template)]
#[template(path = "takeout.html")]
struct TakeoutTemplate {
    common: PageContext,
    active: &'static str,
}

#[derive(Template)]
#[template(path = "menu.html")]
struct MenuTemplate {
    common: PageContext,
    active: &'static str,
    categories: Vec<MenuCategoryView>,
}

#[derive(Template)]
#[template(path = "status.html")]
struct StatusTemplate {
    server_time: String,
    user_name: String,
}

#[derive(Debug, Clone)]
struct TableView {
    name: String,
    zone: String,
}

#[derive(Debug, Clone)]
struct MenuCategoryView {
    name: String,
    items: Vec<MenuItemView>,
}

#[derive(Debug, Clone)]
struct MenuItemView {
    id: String,
    name: String,
    description: String,
    price: String,
    is_available: bool,
}

#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub user: String,
    pub pin: String,
}

#[derive(Debug, Deserialize)]
pub struct LogoutForm {
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct ToggleAvailabilityForm {
    pub csrf_token: String,
    pub is_available: bool,
}

/// Sends the browser to the app or the login page.
pub async fn root(State(state): State<AppState>, jar: CookieJar) -> Response {
    match auth::web_session(&state.pool, &jar).await {
        Ok(Some(_)) => Redirect::to("/app").into_response(),
        Ok(None) => Redirect::to("/login").into_response(),
        Err(error) => error.into_response(),
    }
}

/// Renders the PIN login page.
pub async fn login_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    if auth::web_session(&state.pool, &jar).await?.is_some() {
        return Ok(Redirect::to("/app").into_response());
    }

    let html = LoginTemplate {
        error: String::new(),
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Validates the PIN, creates a web session, and redirects to the app.
pub async fn login_submit(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let Some(login) = auth::login(
        &state.pool,
        &form.user,
        &form.pin,
        SessionKind::Web,
        None,
        state.config.session_hours,
    )
    .await?
    else {
        let html = LoginTemplate {
            error: "Usuario o PIN incorrecto.".to_owned(),
        }
        .render()?;

        return Ok((StatusCode::UNAUTHORIZED, Html(html)).into_response());
    };

    let cookie = auth::session_cookie(login.token);

    Ok((jar.add(cookie), Redirect::to("/app")).into_response())
}

/// Renders the waiter/kitchen web app shell.
pub async fn app_page(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let html = AppTemplate {
        common: page_context(&session),
        active: "home",
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Renders the dining table grid.
pub async fn tables_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let rows = sqlx::query(
        "SELECT name, zone
         FROM dining_table
         WHERE business_id = ?1 AND is_active = 1
         ORDER BY zone, name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;

    let tables = rows
        .into_iter()
        .map(|row| {
            use sqlx::Row;
            Ok(TableView {
                name: row.try_get("name")?,
                zone: row
                    .try_get::<Option<String>, _>("zone")?
                    .unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>, sqlx::Error>>()?;

    let html = TablesTemplate {
        common: page_context(&session),
        active: "tables",
        tables,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Renders the takeout order list. Orders arrive with the next phase.
pub async fn takeout_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let html = TakeoutTemplate {
        common: page_context(&session),
        active: "takeout",
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Renders the menu with the sold-out toggle.
pub async fn menu_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let categories = projection::menu(&state.pool, &session.business_id)
        .await?
        .into_iter()
        .map(|category| MenuCategoryView {
            name: category.name,
            items: category
                .items
                .into_iter()
                .map(|item| MenuItemView {
                    id: item.id,
                    name: item.name,
                    description: item.description.unwrap_or_default(),
                    price: format_price(item.price_minor, &item.currency),
                    is_available: item.is_available,
                })
                .collect(),
        })
        .collect();

    let html = MenuTemplate {
        common: page_context(&session),
        active: "menu",
        categories,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Marks a menu item sold out or available again.
///
/// The web app writes the operation, its projection, and an audit event. The
/// desktop receives the change on its next pull.
pub async fn toggle_item_availability(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(item_id): Path<String>,
    Form(form): Form<ToggleAvailabilityForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    if !matches!(
        session.role.as_str(),
        "admin" | "cashier" | "waiter" | "kitchen"
    ) {
        return Err(AppError::Forbidden);
    }

    let item_id = item_id
        .parse::<Uuid>()
        .map_err(|_| AppError::Validation("invalid menu item id".to_owned()))?;

    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM menu_item WHERE id = ?1 AND business_id = ?2
         )",
    )
    .bind(item_id.to_string())
    .bind(&session.business_id)
    .fetch_one(&state.pool)
    .await?;

    if !exists {
        return Err(AppError::NotFound);
    }

    let device_id = snapshot::server_device_id(&state.pool)
        .await?
        .ok_or_else(|| AppError::Internal("the server device is not configured".to_owned()))?;
    let device_id = device_id
        .parse::<DeviceId>()
        .map_err(|_| AppError::CorruptData("invalid server device id".to_owned()))?;
    let branch_id = auth::default_branch_id(&state.pool, &session.business_id).await?;
    let branch_id = branch_id
        .as_deref()
        .map(str::parse::<BranchId>)
        .transpose()
        .map_err(|_| AppError::CorruptData("invalid branch id".to_owned()))?;

    let operation = Operation::new(
        device_id,
        branch_id,
        OperationKind::MenuItemAvailabilityChanged,
        item_id,
        json!({ "is_available": form.is_available }),
        now_ms(),
    );
    let branch_id_value = branch_id.map(|branch_id| branch_id.to_string());

    sync::append_web_operation(
        &state.pool,
        &operation,
        &session.user_id,
        &session.business_id,
        branch_id_value.as_deref(),
        "menu_item_availability_changed",
        json!({ "is_available": form.is_available }),
    )
    .await?;

    Ok(Redirect::to("/app/menu").into_response())
}

fn format_price(minor: i64, currency: &str) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let absolute = minor.unsigned_abs();

    format!("{sign}{}.{:02} {currency}", absolute / 100, absolute % 100)
}

/// HTMX fragment with the server clock and session status.
pub async fn app_status(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Html("Sesión expirada. Vuelve a entrar.").into_response());
    };

    let html = StatusTemplate {
        server_time: format_local_time(now_ms()),
        user_name: session.display_name,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Revokes the web session. Requires the session CSRF token.
pub async fn logout(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<LogoutForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    auth::revoke_session(&state.pool, &session.session_id).await?;

    let jar = jar.remove(auth::removal_cookie());

    Ok((jar, Redirect::to("/login")).into_response())
}

/// Embedded mobile-first stylesheet.
pub async fn stylesheet() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLESHEET,
    )
}

/// Embedded HTMX runtime.
pub async fn htmx() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
        HTMX,
    )
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, header};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::router;
    use crate::test_support;

    async fn body_text(response: Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    fn form_request(uri: &str, body: &str) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body.to_owned()))
            .unwrap()
    }

    async fn web_login(state: &AppState) -> String {
        web_login_as(state, "Admin", "1234").await
    }

    async fn web_login_as(state: &AppState, user: &str, pin: &str) -> String {
        let body = format!("user={user}&pin={pin}");
        let response = router(state.clone())
            .oneshot(form_request("/login", &body))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
    }

    async fn linked_state() -> (AppState, String, String) {
        let state = test_support::state().await;
        let snapshot = crate::snapshot::sample_snapshot();
        let item_id = snapshot.items[0].id.clone();

        crate::snapshot::link(
            &state.pool,
            crate::snapshot::LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot,
            },
        )
        .await
        .unwrap();

        let cookie = web_login_as(&state, "Ana", "2468").await;

        (state, cookie, item_id)
    }

    #[tokio::test]
    async fn login_page_renders_and_root_redirects() {
        let state = test_support::state().await;

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/login")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("Entrar"));
        assert!(body.contains("Nexo Restaurantes"));

        let response = router(state)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");
    }

    #[tokio::test]
    async fn login_sets_cookie_and_app_requires_a_session() {
        let state = test_support::state().await;

        let cookie = web_login(&state).await;

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/app")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("Café Central"));
        assert!(body.contains("Admin"));

        let response = router(state)
            .oneshot(Request::builder().uri("/app").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");
    }

    #[tokio::test]
    async fn logout_requires_csrf_and_revokes_the_session() {
        let state = test_support::state().await;
        let cookie = web_login(&state).await;

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/logout")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("csrf_token=wrong"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let csrf: String = sqlx::query_scalar("SELECT csrf_token FROM auth_session")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/logout")
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf_token={csrf}")))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let revoked: Option<i64> = sqlx::query_scalar("SELECT revoked_at_ms FROM auth_session")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert!(revoked.is_some());
    }

    #[tokio::test]
    async fn status_fragment_reports_the_session() {
        let state = test_support::state().await;
        let cookie = web_login(&state).await;

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/app/status")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("Servidor en línea"));
        assert!(body.contains("Admin"));
    }

    #[tokio::test]
    async fn web_views_render_the_linked_master_data() {
        let (state, cookie, _) = linked_state().await;

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/app/menu")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("Desayunos"));
        assert!(body.contains("Arepa"));
        assert!(body.contains("2.50 USD"));

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .uri("/app/tables")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("Mesa 1"));
        assert!(body.contains("Salón"));

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/app/takeout")
                    .header(header::COOKIE, &cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert!(
            body_text(response)
                .await
                .contains("No hay pedidos para llevar")
        );
    }

    #[tokio::test]
    async fn sold_out_toggle_writes_an_operation_and_updates_the_projection() {
        let (state, cookie, item_id) = linked_state().await;
        let csrf: String = sqlx::query_scalar("SELECT csrf_token FROM auth_session")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/app/menu/items/{item_id}/availability"))
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(format!("csrf_token={csrf}&is_available=false")))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/app/menu"
        );

        let available: bool =
            sqlx::query_scalar("SELECT is_available FROM menu_item WHERE id = ?1")
                .bind(&item_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(!available);

        let kind: String = sqlx::query_scalar("SELECT kind FROM operation_log")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(kind, "menu_item_availability_changed");

        let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_event")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(audits, 1);
    }

    #[tokio::test]
    async fn sold_out_toggle_requires_the_csrf_token() {
        let (state, cookie, item_id) = linked_state().await;

        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/app/menu/items/{item_id}/availability"))
                    .header(header::COOKIE, &cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from("csrf_token=wrong&is_available=false"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
