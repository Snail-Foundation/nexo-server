use std::str::FromStr;

use askama::Template;
use axum::extract::{Form, Path, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use nexo_core::{
    BranchId, DeviceId, DiningTableId, MenuItemId, Operation, OperationKind, OrderId, OrderLineId,
    Uuid,
};
use serde::Deserialize;
use serde_json::{Value, json};

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
    orders: Vec<OrderSummaryView>,
}

#[derive(Template)]
#[template(path = "order.html")]
struct OrderTemplate {
    common: PageContext,
    active: &'static str,
    order: OrderView,
}

#[derive(Template)]
#[template(path = "kitchen.html")]
struct KitchenTemplate {
    common: PageContext,
    active: &'static str,
    orders: Vec<KitchenOrderView>,
}

#[derive(Template)]
#[template(path = "_kitchen_board.html")]
struct KitchenBoardTemplate {
    common: PageContext,
    orders: Vec<KitchenOrderView>,
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
    id: String,
    name: String,
    zone: String,
    order: Option<OrderSummaryView>,
}

#[derive(Debug, Clone)]
struct OrderSummaryView {
    id: String,
    label: String,
    display_code: String,
    status_label: &'static str,
    total: String,
    opened_at: String,
}

#[derive(Debug, Clone)]
struct OrderView {
    id: String,
    label: String,
    display_code: String,
    order_type_label: &'static str,
    status_label: &'static str,
    status_class: &'static str,
    total: String,
    opened_at: String,
    pending_lines: i64,
    ready_lines: i64,
    can_edit: bool,
    rounds: Vec<OrderRoundView>,
    menu: Vec<OrderMenuCategoryView>,
}

#[derive(Debug, Clone)]
struct OrderRoundView {
    round_number: i64,
    lines: Vec<OrderLineView>,
}

#[derive(Debug, Clone)]
struct OrderLineView {
    id: String,
    description: String,
    quantity: i64,
    notes: String,
    status_label: &'static str,
    status_class: &'static str,
    total: String,
    is_voided: bool,
    void_reason: String,
    can_ready: bool,
    can_serve: bool,
    can_void: bool,
}

#[derive(Debug, Clone)]
struct OrderMenuCategoryView {
    name: String,
    items: Vec<OrderMenuItemView>,
}

#[derive(Debug, Clone)]
struct OrderMenuItemView {
    id: String,
    name: String,
    price: String,
}

#[derive(Debug, Clone)]
struct KitchenOrderView {
    id: String,
    label: String,
    display_code: String,
    order_type_label: &'static str,
    opened_at: String,
    lines: Vec<KitchenLineView>,
    has_sent: bool,
}

#[derive(Debug, Clone)]
struct KitchenLineView {
    id: String,
    description: String,
    quantity: i64,
    notes: String,
    round_number: i64,
    status_label: &'static str,
    status_class: &'static str,
    is_sent: bool,
    sent_minutes: i64,
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

#[derive(Debug, Deserialize)]
pub struct CsrfForm {
    pub csrf_token: String,
}

#[derive(Debug, Deserialize)]
pub struct OpenTakeoutForm {
    pub csrf_token: String,
    #[serde(default)]
    pub customer_name: String,
}

#[derive(Debug, Deserialize)]
pub struct AddLineForm {
    pub csrf_token: String,
    pub menu_item_id: String,
    pub quantity: i64,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Deserialize)]
pub struct VoidLineForm {
    pub csrf_token: String,
    #[serde(default)]
    pub reason: String,
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

/// Renders the dining table grid with open orders.
pub async fn tables_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let tables = load_tables(&state, &session).await?;

    let html = TablesTemplate {
        common: page_context(&session),
        active: "tables",
        tables,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Opens a dine-in order for a free table.
pub async fn open_table(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(table_id): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier", "waiter"])?;

    let table_id = table_id
        .parse::<DiningTableId>()
        .map_err(|_| AppError::Validation("invalid table id".to_owned()))?;

    let table = sqlx::query(
        "SELECT branch_id FROM dining_table
         WHERE id = ?1 AND business_id = ?2 AND is_active = 1",
    )
    .bind(table_id.to_string())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(table) = table else {
        return Err(AppError::NotFound);
    };

    use sqlx::Row;
    let branch_id: String = table.try_get("branch_id")?;
    let branch_id = branch_id
        .parse::<BranchId>()
        .map_err(|_| AppError::CorruptData("invalid branch id".to_owned()))?;

    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM restaurant_order
         WHERE table_id = ?1 AND status IN ('open', 'active', 'served')
         LIMIT 1",
    )
    .bind(table_id.to_string())
    .fetch_optional(&state.pool)
    .await?;
    if let Some(order_id) = existing {
        return Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response());
    }

    let device_id = server_device(&state).await?;
    let display_code = next_display_code(&state.pool, &branch_id.to_string(), "M").await?;
    let order_id = OrderId::new();
    let payload = json!({
        "order_type": "dine_in",
        "table_id": table_id.to_string(),
        "display_code": display_code,
        "opened_by": session.user_id,
        "opened_at_ms": now_ms(),
    });

    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderOpened,
        order_id.into_uuid(),
        payload,
        "order_opened",
    )
    .await?;

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Renders the takeout order list and creation form.
pub async fn takeout_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let orders = load_open_takeout(&state, &session).await?;

    let html = TakeoutTemplate {
        common: page_context(&session),
        active: "takeout",
        orders,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Creates a takeout order with an optional customer name.
pub async fn open_takeout(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<OpenTakeoutForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier", "waiter"])?;

    let branch_id = business_branch(&state, &session).await?;
    let device_id = server_device(&state).await?;
    let display_code = next_display_code(&state.pool, &branch_id.to_string(), "L").await?;
    let order_id = OrderId::new();
    let customer_name = form.customer_name.trim();
    let mut payload = json!({
        "order_type": "takeout",
        "display_code": display_code,
        "opened_by": session.user_id,
        "opened_at_ms": now_ms(),
    });
    if !customer_name.is_empty() {
        payload["customer_name"] = json!(customer_name);
    }

    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderOpened,
        order_id.into_uuid(),
        payload,
        "order_opened",
    )
    .await?;

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Renders one order with its rounds, lines, and actions.
pub async fn order_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let order = order_view(&state, &session, detail).await?;

    let html = OrderTemplate {
        common: page_context(&session),
        active: "tables",
        order,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Adds a pending line to the current round of an order.
pub async fn add_order_line(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
    Form(form): Form<AddLineForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier", "waiter"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !matches!(detail.order.status.as_str(), "open" | "active" | "served") {
        return Err(AppError::Conflict(
            "the order cannot receive new lines".to_owned(),
        ));
    }

    if !(1..=999).contains(&form.quantity) {
        return Err(AppError::Validation(
            "quantity must be greater than zero".to_owned(),
        ));
    }

    let menu_item_id = form
        .menu_item_id
        .parse::<MenuItemId>()
        .map_err(|_| AppError::Validation("invalid menu item id".to_owned()))?;
    let item = sqlx::query(
        "SELECT name, price_minor, currency, is_active, is_available
         FROM menu_item WHERE id = ?1 AND business_id = ?2",
    )
    .bind(menu_item_id.to_string())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some(item) = item else {
        return Err(AppError::NotFound);
    };

    use sqlx::Row;
    let name: String = item.try_get("name")?;
    let price_minor: i64 = item.try_get("price_minor")?;
    let currency: String = item.try_get("currency")?;
    let is_active: bool = item.try_get("is_active")?;
    let is_available: bool = item.try_get("is_available")?;
    if !is_active {
        return Err(AppError::Validation(
            "the menu item is not active".to_owned(),
        ));
    }
    if !is_available {
        return Err(AppError::Validation("the menu item is sold out".to_owned()));
    }

    let pending_round: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(round_number) FROM order_line
         WHERE order_id = ?1 AND status = 'pending'",
    )
    .bind(&order_id)
    .fetch_one(&state.pool)
    .await?;
    let round_number = match pending_round {
        Some(round) => round,
        None => {
            let last: i64 = sqlx::query_scalar(
                "SELECT COALESCE(MAX(round_number), 0) FROM order_line WHERE order_id = ?1",
            )
            .bind(&order_id)
            .fetch_one(&state.pool)
            .await?;
            last + 1
        }
    };

    let line_total_minor = price_minor
        .checked_mul(form.quantity)
        .ok_or_else(|| AppError::Validation("the line total is too large".to_owned()))?;
    let notes = form.notes.trim();
    let line_id = OrderLineId::new();
    let payload = json!({
        "order_id": order_id,
        "menu_item_id": menu_item_id.to_string(),
        "description": name,
        "quantity": form.quantity,
        "unit_price_minor": price_minor,
        "line_total_minor": line_total_minor,
        "currency": currency,
        "notes": (!notes.is_empty()).then_some(notes),
        "round_number": round_number,
        "created_by": session.user_id,
        "created_at_ms": now_ms(),
    });

    let branch_id = order_branch(&state, &session, &order_id).await?;
    let device_id = server_device(&state).await?;

    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderLineAdded,
        line_id.into_uuid(),
        payload,
        "order_line_added",
    )
    .await?;

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Sends every pending line of the current round to the kitchen.
pub async fn send_round(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier", "waiter"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    if !matches!(detail.order.status.as_str(), "open" | "active" | "served") {
        return Err(AppError::Conflict(
            "the order cannot be sent to the kitchen".to_owned(),
        ));
    }

    let round_number = detail
        .lines
        .iter()
        .filter(|line| line.status == "pending")
        .map(|line| line.round_number)
        .max();
    let Some(round_number) = round_number else {
        return Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response());
    };

    let line_ids = detail
        .lines
        .iter()
        .filter(|line| line.status == "pending" && line.round_number == round_number)
        .map(|line| line.id.clone())
        .collect::<Vec<_>>();
    let payload = json!({
        "round_number": round_number,
        "line_ids": line_ids,
        "sent_by": session.user_id,
        "sent_at_ms": now_ms(),
    });

    let branch_id = order_branch(&state, &session, &order_id).await?;
    let device_id = server_device(&state).await?;

    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderSent,
        OrderId::from_str(&order_id)
            .map_err(|_| AppError::CorruptData("invalid order id".to_owned()))?
            .into_uuid(),
        payload,
        "order_sent",
    )
    .await?;

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Voids one pending, sent, or ready line with a reason.
pub async fn void_line(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((order_id, line_id)): Path<(String, String)>,
    Form(form): Form<VoidLineForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier", "waiter"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let line_id = line_id
        .parse::<OrderLineId>()
        .map_err(|_| AppError::Validation("invalid order line id".to_owned()))?
        .to_string();
    let reason = form.reason.trim();
    if reason.is_empty() {
        return Err(AppError::Validation("a void reason is required".to_owned()));
    }

    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let line = detail
        .lines
        .iter()
        .find(|line| line.id == line_id)
        .ok_or(AppError::NotFound)?;
    if !matches!(line.status.as_str(), "pending" | "sent" | "ready") {
        return Err(AppError::Conflict("the line cannot be voided".to_owned()));
    }

    let payload = json!({
        "order_id": order_id,
        "reason": reason,
        "voided_by": session.user_id,
        "voided_at_ms": now_ms(),
    });

    let branch_id = order_branch(&state, &session, &order_id).await?;
    let device_id = server_device(&state).await?;

    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderLineVoided,
        OrderLineId::from_str(&line_id)
            .map_err(|_| AppError::CorruptData("invalid order line id".to_owned()))?
            .into_uuid(),
        payload,
        "order_line_voided",
    )
    .await?;

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Marks one sent line ready from the kitchen.
pub async fn mark_line_ready(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Path((order_id, line_id)): Path<(String, String)>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "kitchen"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let line_id = line_id
        .parse::<OrderLineId>()
        .map_err(|_| AppError::Validation("invalid order line id".to_owned()))?
        .to_string();

    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let line = detail
        .lines
        .iter()
        .find(|line| line.id == line_id)
        .ok_or(AppError::NotFound)?;
    if line.status != "sent" {
        return Err(AppError::Conflict(
            "the line cannot be marked ready".to_owned(),
        ));
    }

    let payload = json!({
        "order_id": order_id,
        "ready_at_ms": now_ms(),
    });

    let branch_id = order_branch(&state, &session, &order_id).await?;
    let device_id = server_device(&state).await?;

    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderLineReady,
        OrderLineId::from_str(&line_id)
            .map_err(|_| AppError::CorruptData("invalid order line id".to_owned()))?
            .into_uuid(),
        payload,
        "order_line_ready",
    )
    .await?;

    if headers.contains_key("hx-request") {
        return kitchen_board_response(&state, &session).await;
    }

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Marks every sent line of an order ready.
pub async fn mark_order_ready(
    State(state): State<AppState>,
    jar: CookieJar,
    headers: HeaderMap,
    Path(order_id): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "kitchen"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let line_ids = detail
        .lines
        .iter()
        .filter(|line| line.status == "sent")
        .map(|line| line.id.clone())
        .collect::<Vec<_>>();

    let branch_id = order_branch(&state, &session, &order_id).await?;
    let device_id = server_device(&state).await?;
    let ready_at_ms = now_ms();

    for line_id in line_ids {
        let payload = json!({
            "order_id": order_id,
            "ready_at_ms": ready_at_ms,
        });
        append_order_operation(
            &state,
            &session,
            device_id,
            branch_id,
            OperationKind::OrderLineReady,
            OrderLineId::from_str(&line_id)
                .map_err(|_| AppError::CorruptData("invalid order line id".to_owned()))?
                .into_uuid(),
            payload,
            "order_line_ready",
        )
        .await?;
    }

    if headers.contains_key("hx-request") {
        return kitchen_board_response(&state, &session).await;
    }

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Marks one sent or ready line served from the waiter view.
pub async fn mark_line_served(
    State(state): State<AppState>,
    jar: CookieJar,
    Path((order_id, line_id)): Path<(String, String)>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier", "waiter"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let line_id = line_id
        .parse::<OrderLineId>()
        .map_err(|_| AppError::Validation("invalid order line id".to_owned()))?
        .to_string();

    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let line = detail
        .lines
        .iter()
        .find(|line| line.id == line_id)
        .ok_or(AppError::NotFound)?;
    if !matches!(line.status.as_str(), "sent" | "ready") {
        return Err(AppError::Conflict(
            "the line cannot be marked served".to_owned(),
        ));
    }

    let payload = json!({
        "order_id": order_id,
        "served_at_ms": now_ms(),
    });

    let branch_id = order_branch(&state, &session, &order_id).await?;
    let device_id = server_device(&state).await?;

    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderLineServed,
        OrderLineId::from_str(&line_id)
            .map_err(|_| AppError::CorruptData("invalid order line id".to_owned()))?
            .into_uuid(),
        payload,
        "order_line_served",
    )
    .await?;

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Marks every sent or ready line of an order served.
pub async fn mark_order_served(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
    Form(form): Form<CsrfForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier", "waiter"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let line_ids = detail
        .lines
        .iter()
        .filter(|line| matches!(line.status.as_str(), "sent" | "ready"))
        .map(|line| line.id.clone())
        .collect::<Vec<_>>();

    let branch_id = order_branch(&state, &session, &order_id).await?;
    let device_id = server_device(&state).await?;
    let served_at_ms = now_ms();

    for line_id in line_ids {
        let payload = json!({
            "order_id": order_id,
            "served_at_ms": served_at_ms,
        });
        append_order_operation(
            &state,
            &session,
            device_id,
            branch_id,
            OperationKind::OrderLineServed,
            OrderLineId::from_str(&line_id)
                .map_err(|_| AppError::CorruptData("invalid order line id".to_owned()))?
                .into_uuid(),
            payload,
            "order_line_served",
        )
        .await?;
    }

    Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response())
}

/// Renders the kitchen board.
pub async fn kitchen_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let orders = load_kitchen_orders(&state, &session).await?;

    let html = KitchenTemplate {
        common: page_context(&session),
        active: "kitchen",
        orders,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// HTMX fragment with the current kitchen board.
pub async fn kitchen_board(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Html("Sesión expirada. Vuelve a entrar.").into_response());
    };

    kitchen_board_response(&state, &session).await
}

async fn kitchen_board_response(
    state: &AppState,
    session: &SessionUser,
) -> Result<Response, AppError> {
    let orders = load_kitchen_orders(state, session).await?;
    let html = KitchenBoardTemplate {
        common: page_context(session),
        orders,
    }
    .render()?;

    Ok(Html(html).into_response())
}

async fn load_tables(state: &AppState, session: &SessionUser) -> Result<Vec<TableView>, AppError> {
    let currency = base_currency(&state.pool, &session.business_id).await?;
    let rows = sqlx::query(
        "SELECT
            t.id, t.name, t.zone,
            o.id AS order_id, o.display_code, o.status, o.opened_at_ms,
            COALESCE(SUM(
                CASE WHEN l.status <> 'voided' THEN l.line_total_minor ELSE 0 END
            ), 0) AS total_minor
         FROM dining_table t
         LEFT JOIN restaurant_order o
           ON o.table_id = t.id AND o.status IN ('open', 'active', 'served')
         LEFT JOIN order_line l ON l.order_id = o.id
         WHERE t.business_id = ?1 AND t.is_active = 1
         GROUP BY t.id
         ORDER BY t.zone, t.name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;

    use sqlx::Row;
    let mut tables = Vec::with_capacity(rows.len());

    for row in rows {
        let order_id: Option<String> = row.try_get("order_id")?;
        let order = match order_id {
            Some(id) => {
                let status: String = row.try_get("status")?;
                let total_minor: i64 = row.try_get("total_minor")?;
                let opened_at_ms: i64 = row.try_get("opened_at_ms")?;
                let display_code: String = row.try_get("display_code")?;
                Some(OrderSummaryView {
                    id,
                    label: row.try_get::<String, _>("name")?,
                    display_code,
                    status_label: order_status_label(&status),
                    total: format_price(total_minor, &currency),
                    opened_at: format_local_time(opened_at_ms),
                })
            }
            None => None,
        };

        tables.push(TableView {
            id: row.try_get("id")?,
            name: row.try_get("name")?,
            zone: row
                .try_get::<Option<String>, _>("zone")?
                .unwrap_or_default(),
            order,
        });
    }

    Ok(tables)
}

async fn load_open_takeout(
    state: &AppState,
    session: &SessionUser,
) -> Result<Vec<OrderSummaryView>, AppError> {
    let currency = base_currency(&state.pool, &session.business_id).await?;

    Ok(projection::open_orders(&state.pool, &session.business_id)
        .await?
        .into_iter()
        .filter(|order| order.order_type == "takeout")
        .map(|order| order_summary_view(&order, &currency))
        .collect())
}

fn order_summary_view(order: &projection::ProjectedOrder, currency: &str) -> OrderSummaryView {
    OrderSummaryView {
        id: order.id.clone(),
        label: order_label(
            &order.order_type,
            order.table_name.as_deref(),
            order.customer_name.as_deref(),
            &order.display_code,
        ),
        display_code: order.display_code.clone(),
        status_label: order_status_label(&order.status),
        total: format_price(order.total_minor, currency),
        opened_at: format_local_time(order.opened_at_ms),
    }
}

async fn order_view(
    state: &AppState,
    session: &SessionUser,
    detail: projection::ProjectedOrderDetail,
) -> Result<OrderView, AppError> {
    let currency = base_currency(&state.pool, &session.business_id).await?;
    let can_edit_role = matches!(session.role.as_str(), "admin" | "cashier" | "waiter");
    let can_kitchen = matches!(session.role.as_str(), "admin" | "kitchen");
    let is_terminal = matches!(detail.order.status.as_str(), "billed" | "cancelled");
    let can_edit = can_edit_role && !is_terminal;
    let ready_lines = detail
        .lines
        .iter()
        .filter(|line| line.status == "ready")
        .count() as i64;

    let mut rounds: Vec<OrderRoundView> = Vec::new();
    for line in detail.lines {
        let line_view = OrderLineView {
            id: line.id.clone(),
            description: line.description,
            quantity: line.quantity,
            notes: line.notes.unwrap_or_default(),
            status_label: line_status_label(&line.status),
            status_class: line_status_class(&line.status),
            total: format_price(line.line_total_minor, &currency),
            is_voided: line.status == "voided",
            void_reason: line.void_reason.unwrap_or_default(),
            can_ready: can_kitchen && line.status == "sent",
            can_serve: can_edit && matches!(line.status.as_str(), "sent" | "ready"),
            can_void: can_edit && matches!(line.status.as_str(), "pending" | "sent" | "ready"),
        };

        match rounds.last_mut() {
            Some(round) if round.round_number == line.round_number => {
                round.lines.push(line_view);
            }
            _ => rounds.push(OrderRoundView {
                round_number: line.round_number,
                lines: vec![line_view],
            }),
        }
    }

    let menu = if can_edit {
        load_order_menu(state, session).await?
    } else {
        Vec::new()
    };

    Ok(OrderView {
        id: detail.order.id.clone(),
        label: order_label(
            &detail.order.order_type,
            detail.order.table_name.as_deref(),
            detail.order.customer_name.as_deref(),
            &detail.order.display_code,
        ),
        display_code: detail.order.display_code.clone(),
        order_type_label: order_type_label(&detail.order.order_type),
        status_label: order_status_label(&detail.order.status),
        status_class: order_status_class(&detail.order.status),
        total: format_price(detail.order.total_minor, &currency),
        opened_at: format_local_time(detail.order.opened_at_ms),
        pending_lines: detail.order.pending_lines,
        ready_lines,
        can_edit,
        rounds,
        menu,
    })
}

async fn load_order_menu(
    state: &AppState,
    session: &SessionUser,
) -> Result<Vec<OrderMenuCategoryView>, AppError> {
    Ok(projection::menu(&state.pool, &session.business_id)
        .await?
        .into_iter()
        .filter_map(|category| {
            let items = category
                .items
                .into_iter()
                .filter(|item| item.is_active && item.is_available)
                .map(|item| OrderMenuItemView {
                    id: item.id,
                    name: item.name,
                    price: format_price(item.price_minor, &item.currency),
                })
                .collect::<Vec<_>>();

            (!items.is_empty()).then_some(OrderMenuCategoryView {
                name: category.name,
                items,
            })
        })
        .collect())
}

async fn load_kitchen_orders(
    state: &AppState,
    session: &SessionUser,
) -> Result<Vec<KitchenOrderView>, AppError> {
    let now = now_ms();

    Ok(
        projection::kitchen_orders(&state.pool, &session.business_id)
            .await?
            .into_iter()
            .map(|detail| {
                let has_sent = detail.lines.iter().any(|line| line.status == "sent");
                let lines = detail
                    .lines
                    .into_iter()
                    .map(|line| KitchenLineView {
                        id: line.id,
                        description: line.description,
                        quantity: line.quantity,
                        notes: line.notes.unwrap_or_default(),
                        round_number: line.round_number,
                        status_label: line_status_label(&line.status),
                        status_class: line_status_class(&line.status),
                        is_sent: line.status == "sent",
                        sent_minutes: line
                            .sent_at_ms
                            .map(|sent| (now - sent).max(0) / 60_000)
                            .unwrap_or(0),
                    })
                    .collect();

                KitchenOrderView {
                    id: detail.order.id,
                    label: order_label(
                        &detail.order.order_type,
                        detail.order.table_name.as_deref(),
                        detail.order.customer_name.as_deref(),
                        &detail.order.display_code,
                    ),
                    display_code: detail.order.display_code,
                    order_type_label: order_type_label(&detail.order.order_type),
                    opened_at: format_local_time(detail.order.opened_at_ms),
                    lines,
                    has_sent,
                }
            })
            .collect(),
    )
}

fn order_label(
    order_type: &str,
    table_name: Option<&str>,
    customer_name: Option<&str>,
    display_code: &str,
) -> String {
    match order_type {
        "dine_in" => table_name.unwrap_or(display_code).to_owned(),
        _ => customer_name
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(display_code)
            .to_owned(),
    }
}

fn order_type_label(order_type: &str) -> &'static str {
    match order_type {
        "dine_in" => "Salón",
        _ => "Para llevar",
    }
}

fn order_status_label(status: &str) -> &'static str {
    match status {
        "open" => "Abierto",
        "active" => "En curso",
        "served" => "Servido",
        "billed" => "Cobrado",
        "cancelled" => "Cancelado",
        _ => "Desconocido",
    }
}

fn order_status_class(status: &str) -> &'static str {
    match status {
        "active" | "served" => "ok",
        "billed" => "neutral",
        "cancelled" => "off",
        _ => "warn",
    }
}

fn line_status_label(status: &str) -> &'static str {
    match status {
        "pending" => "Pendiente",
        "sent" => "En cocina",
        "ready" => "Listo",
        "served" => "Servido",
        "voided" => "Anulado",
        _ => "Desconocido",
    }
}

fn line_status_class(status: &str) -> &'static str {
    match status {
        "ready" | "served" => "ok",
        "voided" => "off",
        "sent" => "warn",
        _ => "neutral",
    }
}

async fn base_currency(pool: &sqlx::SqlitePool, business_id: &str) -> Result<String, AppError> {
    Ok(
        sqlx::query_scalar("SELECT base_currency FROM business WHERE id = ?1")
            .bind(business_id)
            .fetch_optional(pool)
            .await?
            .unwrap_or_else(|| "USD".to_owned()),
    )
}

async fn server_device(state: &AppState) -> Result<DeviceId, AppError> {
    snapshot::server_device_id(&state.pool)
        .await?
        .ok_or_else(|| AppError::Internal("the server device is not configured".to_owned()))?
        .parse::<DeviceId>()
        .map_err(|_| AppError::CorruptData("invalid server device id".to_owned()))
}

async fn business_branch(state: &AppState, session: &SessionUser) -> Result<BranchId, AppError> {
    auth::default_branch_id(&state.pool, &session.business_id)
        .await?
        .ok_or_else(|| AppError::Validation("the business has no branch".to_owned()))?
        .parse::<BranchId>()
        .map_err(|_| AppError::CorruptData("invalid branch id".to_owned()))
}

async fn order_branch(
    state: &AppState,
    session: &SessionUser,
    order_id: &str,
) -> Result<BranchId, AppError> {
    let branch_id: String = sqlx::query_scalar(
        "SELECT branch_id FROM restaurant_order WHERE id = ?1 AND business_id = ?2",
    )
    .bind(order_id)
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    branch_id
        .parse::<BranchId>()
        .map_err(|_| AppError::CorruptData("invalid branch id".to_owned()))
}

async fn next_display_code(
    pool: &sqlx::SqlitePool,
    branch_id: &str,
    prefix: &str,
) -> Result<String, AppError> {
    let pattern = format!("{prefix}-%");
    let last: Option<i64> = sqlx::query_scalar(
        "SELECT MAX(CAST(SUBSTR(display_code, 3) AS INTEGER))
         FROM restaurant_order
         WHERE branch_id = ?1 AND display_code LIKE ?2",
    )
    .bind(branch_id)
    .bind(pattern)
    .fetch_one(pool)
    .await?;

    Ok(format!("{prefix}-{:03}", last.unwrap_or(0) + 1))
}

fn require_roles(session: &SessionUser, roles: &[&str]) -> Result<(), AppError> {
    if roles.contains(&session.role.as_str()) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

#[allow(clippy::too_many_arguments)]
async fn append_order_operation(
    state: &AppState,
    session: &SessionUser,
    device_id: DeviceId,
    branch_id: BranchId,
    kind: OperationKind,
    entity_id: Uuid,
    payload: Value,
    action: &str,
) -> Result<(), AppError> {
    let operation = Operation::new(
        device_id,
        Some(branch_id),
        kind,
        entity_id,
        payload.clone(),
        now_ms(),
    );
    let branch_value = branch_id.to_string();

    sync::append_web_operation(
        &state.pool,
        &operation,
        &session.user_id,
        &session.business_id,
        Some(branch_value.as_str()),
        action,
        payload,
    )
    .await
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

    async fn get_page(state: &AppState, uri: &str, cookie: &str) -> Response {
        router(state.clone())
            .oneshot(
                Request::builder()
                    .uri(uri)
                    .header(header::COOKIE, cookie)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn post_form(state: &AppState, uri: &str, cookie: &str, body: &str) -> Response {
        router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header(header::COOKIE, cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap()
    }

    async fn csrf_for(state: &AppState, user: &str) -> String {
        sqlx::query_scalar(
            "SELECT s.csrf_token
             FROM auth_session s
             JOIN app_user u ON u.id = s.user_id
             WHERE u.display_name = ?1 AND s.revoked_at_ms IS NULL
             ORDER BY s.created_at_ms DESC
             LIMIT 1",
        )
        .bind(user)
        .fetch_one(&state.pool)
        .await
        .unwrap()
    }

    async fn add_user(state: &AppState, name: &str, role: &str, pin: &str) -> String {
        let business_id: String =
            sqlx::query_scalar("SELECT id FROM business ORDER BY created_at_ms LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let id = Uuid::now_v7().to_string();
        let now = now_ms();

        sqlx::query(
            "INSERT INTO app_user (
                id, business_id, display_name, role, pin_hash, is_active,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, 1, ?6, ?6)",
        )
        .bind(&id)
        .bind(&business_id)
        .bind(name)
        .bind(role)
        .bind(auth::hash_pin(pin).unwrap())
        .bind(now)
        .execute(&state.pool)
        .await
        .unwrap();

        id
    }

    async fn open_sent_order(state: &AppState, cookie: &str, item_id: &str) -> (String, String) {
        let csrf = csrf_for(state, "Ana").await;
        let table_id: String = sqlx::query_scalar("SELECT id FROM dining_table LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let response = post_form(
            state,
            &format!("/app/tables/{table_id}/open"),
            cookie,
            &format!("csrf_token={csrf}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let order_id = location.rsplit('/').next().unwrap().to_owned();

        let response = post_form(
            state,
            &format!("/app/orders/{order_id}/lines"),
            cookie,
            &format!("csrf_token={csrf}&menu_item_id={item_id}&quantity=2&notes=Sin+queso"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let line_id: String = sqlx::query_scalar("SELECT id FROM order_line WHERE order_id = ?1")
            .bind(&order_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();

        let response = post_form(
            state,
            &format!("/app/orders/{order_id}/send"),
            cookie,
            &format!("csrf_token={csrf}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        (order_id, line_id)
    }

    #[tokio::test]
    async fn waiter_opens_a_table_adds_a_line_and_sends_the_round() {
        let (state, cookie, item_id) = linked_state().await;
        let (order_id, line_id) = open_sent_order(&state, &cookie, &item_id).await;

        let line: (String, i64, i64, Option<String>) = sqlx::query_as(
            "SELECT status, round_number, line_total_minor, notes
             FROM order_line WHERE id = ?1",
        )
        .bind(&line_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(line.0, "sent");
        assert_eq!(line.1, 1);
        assert_eq!(line.2, 500);
        assert_eq!(line.3.as_deref(), Some("Sin queso"));

        let order_status: String =
            sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
                .bind(&order_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(order_status, "active");

        let body =
            body_text(get_page(&state, &format!("/app/orders/{order_id}"), &cookie).await).await;
        assert!(body.contains("Ronda 1"));
        assert!(body.contains("Arepa"));
        assert!(body.contains("En cocina"));
        assert!(body.contains("5.00 USD"));

        let kinds: Vec<String> = sqlx::query_scalar("SELECT kind FROM operation_log ORDER BY seq")
            .fetch_all(&state.pool)
            .await
            .unwrap();
        assert_eq!(
            kinds,
            vec!["order_opened", "order_line_added", "order_sent"]
        );

        let audits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_event")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(audits, 3);
    }

    #[tokio::test]
    async fn kitchen_marks_ready_and_waiter_marks_served() {
        let (state, admin_cookie, item_id) = linked_state().await;
        let (order_id, line_id) = open_sent_order(&state, &admin_cookie, &item_id).await;
        add_user(&state, "Cocina", "kitchen", "1357").await;
        let kitchen_cookie = web_login_as(&state, "Cocina", "1357").await;
        let kitchen_csrf = csrf_for(&state, "Cocina").await;

        let board = body_text(get_page(&state, "/app/kitchen", &kitchen_cookie).await).await;
        assert!(board.contains("Mesa 1"));
        assert!(board.contains("Arepa"));
        assert!(board.contains("Ronda 1"));

        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/app/orders/{order_id}/lines/{line_id}/ready"))
                    .header(header::COOKIE, &kitchen_cookie)
                    .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
                    .header("hx-request", "true")
                    .body(Body::from(format!("csrf_token={kitchen_csrf}")))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let board = body_text(response).await;
        assert!(board.contains("Arepa"));
        assert!(board.contains("Listo"));
        assert!(!board.contains(&format!("/app/orders/{order_id}/lines/{line_id}/ready")));

        let line_status: String = sqlx::query_scalar("SELECT status FROM order_line WHERE id = ?1")
            .bind(&line_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(line_status, "ready");

        let admin_csrf = csrf_for(&state, "Ana").await;
        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/lines/{line_id}/served"),
            &admin_cookie,
            &format!("csrf_token={admin_csrf}"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let line_status: String = sqlx::query_scalar("SELECT status FROM order_line WHERE id = ?1")
            .bind(&line_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let order_status: String =
            sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
                .bind(&order_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(line_status, "served");
        assert_eq!(order_status, "served");
    }

    #[tokio::test]
    async fn takeout_orders_are_created_with_a_code_and_customer() {
        let (state, cookie, _) = linked_state().await;
        let csrf = csrf_for(&state, "Ana").await;

        let response = post_form(
            &state,
            "/app/takeout",
            &cookie,
            &format!("csrf_token={csrf}&customer_name=Carla"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let location = response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(location.starts_with("/app/orders/"));

        let order: (String, Option<String>, String) =
            sqlx::query_as("SELECT order_type, customer_name, display_code FROM restaurant_order")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(order.0, "takeout");
        assert_eq!(order.1.as_deref(), Some("Carla"));
        assert_eq!(order.2, "L-001");

        let body = body_text(get_page(&state, "/app/takeout", &cookie).await).await;
        assert!(body.contains("Carla"));
        assert!(body.contains("L-001"));
    }

    #[tokio::test]
    async fn order_actions_require_csrf_and_the_right_role() {
        let (state, admin_cookie, item_id) = linked_state().await;
        let (order_id, _) = open_sent_order(&state, &admin_cookie, &item_id).await;
        add_user(&state, "Cocina", "kitchen", "1357").await;
        let kitchen_cookie = web_login_as(&state, "Cocina", "1357").await;
        let kitchen_csrf = csrf_for(&state, "Cocina").await;

        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/lines"),
            &kitchen_cookie,
            &format!("csrf_token={kitchen_csrf}&menu_item_id={item_id}&quantity=1"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let table_id: String = sqlx::query_scalar("SELECT id FROM dining_table LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let response = post_form(
            &state,
            &format!("/app/tables/{table_id}/open"),
            &admin_cookie,
            "csrf_token=wrong",
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
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
