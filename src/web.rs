use std::str::FromStr;

use askama::Template;
use axum::body::Bytes;
use axum::extract::{Form, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use nexo_core::{
    BranchId, CashMovementId, CashSessionId, DeviceId, DiningTableId, MenuItemId, Operation,
    OperationKind, OrderId, OrderLineId, PaymentId, SaleId, Uuid,
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::{self, SessionKind, SessionUser};
use crate::billing;
use crate::error::AppError;
use crate::projection;
use crate::state::AppState;
use crate::sync::{self, WebOperation};
use crate::time::{
    format_local_datetime, format_local_time, local_day_bounds, now_ms, today_local_date,
};

const STYLESHEET: &str = include_str!("../static/app.css");
const HTMX: &str = include_str!("../static/htmx.min.js");
const MANIFEST: &str = include_str!("../static/manifest.webmanifest");
const ICON_SVG: &str = include_str!("../static/icon.svg");
const ICON_192: &[u8] = include_bytes!("../static/icon-192.png");
const ICON_512: &[u8] = include_bytes!("../static/icon-512.png");
const APPLE_TOUCH_ICON: &[u8] = include_bytes!("../static/apple-touch-icon.png");

#[derive(Clone)]
struct PageContext {
    business_name: String,
    user_name: String,
    role_label: String,
    csrf_token: String,
    version: &'static str,
    can_serve: bool,
    can_kitchen: bool,
    can_manage: bool,
    can_admin: bool,
}

fn page_context(session: &SessionUser) -> PageContext {
    PageContext {
        business_name: session.business_name.clone(),
        user_name: session.display_name.clone(),
        role_label: auth::role_label(&session.role).to_owned(),
        csrf_token: session.csrf_token.clone(),
        version: env!("CARGO_PKG_VERSION"),
        can_serve: auth::can_serve(&session.role),
        can_kitchen: auth::can_kitchen(&session.role),
        can_manage: auth::can_manage(&session.role),
        can_admin: session.role == "admin",
    }
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: String,
}

#[derive(Template)]
#[template(path = "forbidden.html")]
struct ForbiddenTemplate;

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorTemplate {
    status: u16,
    title: &'static str,
    message: &'static str,
    back_href: String,
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
#[template(path = "order_status.html")]
struct OrderStatusTemplate {
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
    can_cancel: bool,
    active: &'static str,
    back_href: &'static str,
    back_label: &'static str,
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

#[derive(Template)]
#[template(path = "cashier.html")]
struct CashierTemplate {
    common: PageContext,
    active: &'static str,
    session: Option<CashSessionView>,
    orders: Vec<OrderSummaryView>,
}

#[derive(Debug, Clone)]
struct CashSessionView {
    opened_at: String,
    opened_by: String,
    status_label: &'static str,
    base_currency: String,
    secondary_currency: String,
    opening_base: String,
    opening_secondary: String,
    expected_base: String,
    expected_secondary: String,
    movements: Vec<CashMovementView>,
}

#[derive(Debug, Clone)]
struct CashMovementView {
    kind_label: &'static str,
    amount: String,
    reason: String,
    created_at: String,
}

#[derive(Template)]
#[template(path = "bill.html")]
struct BillTemplate {
    common: PageContext,
    active: &'static str,
    order: BillOrderView,
    methods: Vec<PaymentMethodView>,
    base_currency: String,
    secondary_currency: String,
    exchange_rate: String,
    tax_enabled: bool,
    tax_name: String,
    tax_rate_label: String,
    subtotal: String,
    tax: String,
    total: String,
    has_cash_session: bool,
}

#[derive(Debug, Clone)]
struct BillOrderView {
    id: String,
    label: String,
    display_code: String,
    order_type_label: &'static str,
    opened_at: String,
    lines: Vec<BillLineView>,
}

#[derive(Debug, Clone)]
struct BillLineView {
    description: String,
    quantity: i64,
    total: String,
    notes: String,
}

#[derive(Debug, Clone)]
struct PaymentMethodView {
    id: String,
    name: String,
    kind_label: &'static str,
    currency: String,
}

#[derive(Template)]
#[template(path = "receipt.html")]
struct ReceiptTemplate {
    common: PageContext,
    active: &'static str,
    sale: ReceiptView,
}

#[derive(Debug, Clone)]
struct ReceiptView {
    number: String,
    business_name: String,
    order_label: String,
    display_code: String,
    created_at: String,
    cashier: String,
    status_label: &'static str,
    subtotal: String,
    tax: String,
    tax_label: String,
    total: String,
    has_tax: bool,
    lines: Vec<ReceiptLineView>,
    payments: Vec<ReceiptPaymentView>,
}

#[derive(Debug, Clone)]
struct ReceiptLineView {
    description: String,
    quantity: i64,
    total: String,
}

#[derive(Debug, Clone)]
struct ReceiptPaymentView {
    method: String,
    amount: String,
    tendered: String,
    change: String,
    reference: String,
    exchange_rate: String,
}

#[derive(Template)]
#[template(path = "cash.html")]
struct CashTemplate {
    common: PageContext,
    active: &'static str,
    session: Option<CashSessionView>,
    base_currency: String,
    secondary_currency: String,
}

#[derive(Template)]
#[template(path = "reports.html")]
struct ReportsTemplate {
    common: PageContext,
    active: &'static str,
    date: String,
    base_currency: String,
    sale_count: i64,
    subtotal: String,
    tax: String,
    tax_label: String,
    total: String,
    average: String,
    has_tax: bool,
    payments: Vec<ReportPaymentView>,
    top_items: Vec<ReportItemView>,
    sales: Vec<ReportSaleView>,
    cash_sessions: Vec<ReportCashView>,
}

#[derive(Debug, Clone)]
struct ReportPaymentView {
    method: String,
    kind_label: &'static str,
    amount: String,
    base_amount: String,
    count: i64,
}

#[derive(Debug, Clone)]
struct ReportItemView {
    description: String,
    quantity: i64,
    total: String,
}

#[derive(Debug, Clone)]
struct ReportSaleView {
    id: String,
    number: String,
    time: String,
    label: String,
    type_label: &'static str,
    total: String,
    status_label: &'static str,
}

#[derive(Debug, Clone)]
struct ReportCashView {
    device: String,
    opened_at: String,
    closed_at: String,
    status_label: &'static str,
    base_currency: String,
    secondary_currency: String,
    opening_base: String,
    expected_base: String,
    counted_base: String,
    difference_base: String,
    opening_secondary: String,
    expected_secondary: String,
    counted_secondary: String,
    difference_secondary: String,
}

#[derive(Debug, Deserialize)]
pub struct ReportQuery {
    #[serde(default)]
    pub date: String,
}

#[derive(Template)]
#[template(path = "admin.html")]
struct AdminTemplate {
    common: PageContext,
    active: &'static str,
    business_name: String,
    legal_name: String,
    tax_id: String,
    base_currency: String,
    exchange_rate: String,
    tax_name: String,
    tax_rate: String,
    tax_enabled: bool,
    categories: Vec<AdminCategoryView>,
    items: Vec<AdminItemView>,
    tables: Vec<AdminTableView>,
    users: Vec<AdminUserView>,
    methods: Vec<AdminMethodView>,
    category_form: AdminCategoryEdit,
    item_form: AdminItemEdit,
    table_form: AdminTableEdit,
    user_form: AdminUserEdit,
    method_form: AdminMethodEdit,
}

#[derive(Debug, Clone)]
struct AdminCategoryView {
    id: String,
    name: String,
    sort_order: i64,
    is_active: bool,
}

#[derive(Debug, Clone)]
struct AdminCategoryEdit {
    id: String,
    name: String,
    sort_order: i64,
    is_active: bool,
}

#[derive(Debug, Clone)]
struct AdminItemView {
    id: String,
    name: String,
    description: String,
    category: String,
    price: String,
    is_active: bool,
    is_available: bool,
}

#[derive(Debug, Clone)]
struct AdminItemEdit {
    id: String,
    name: String,
    description: String,
    category_id: String,
    price: String,
    sort_order: i64,
    is_active: bool,
    is_available: bool,
}

#[derive(Debug, Clone)]
struct AdminTableView {
    id: String,
    name: String,
    zone: String,
    is_active: bool,
    occupied: bool,
}

#[derive(Debug, Clone)]
struct AdminTableEdit {
    id: String,
    name: String,
    zone: String,
    is_active: bool,
}

#[derive(Debug, Clone)]
struct AdminUserView {
    id: String,
    name: String,
    role_label: String,
    is_active: bool,
    has_pin: bool,
}

#[derive(Debug, Clone)]
struct AdminUserEdit {
    id: String,
    name: String,
    role: String,
    is_active: bool,
}

#[derive(Debug, Clone)]
struct AdminMethodView {
    id: String,
    name: String,
    kind_label: &'static str,
    currency: String,
    sort_order: i64,
    is_active: bool,
}

#[derive(Debug, Clone)]
struct AdminMethodEdit {
    id: String,
    name: String,
    kind: String,
    currency: String,
    sort_order: i64,
    is_active: bool,
}

#[derive(Debug, Default, Deserialize)]
pub struct AdminQuery {
    #[serde(default)]
    pub edit_category: String,
    #[serde(default)]
    pub edit_item: String,
    #[serde(default)]
    pub edit_table: String,
    #[serde(default)]
    pub edit_user: String,
    #[serde(default)]
    pub edit_method: String,
}

#[derive(Debug, Deserialize)]
pub struct BusinessAdminForm {
    pub csrf_token: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub legal_name: String,
    #[serde(default)]
    pub tax_id: String,
    #[serde(default)]
    pub exchange_rate: String,
    #[serde(default)]
    pub tax_name: String,
    #[serde(default)]
    pub tax_rate: String,
    #[serde(default)]
    pub tax_enabled: String,
}

#[derive(Debug, Deserialize)]
pub struct CategoryAdminForm {
    pub csrf_token: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub sort_order: String,
    #[serde(default)]
    pub is_active: String,
}

#[derive(Debug, Deserialize)]
pub struct ItemAdminForm {
    pub csrf_token: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub category_id: String,
    #[serde(default)]
    pub price: String,
    #[serde(default)]
    pub sort_order: String,
    #[serde(default)]
    pub is_active: String,
    #[serde(default)]
    pub is_available: String,
}

#[derive(Debug, Deserialize)]
pub struct TableAdminForm {
    pub csrf_token: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub zone: String,
    #[serde(default)]
    pub is_active: String,
}

#[derive(Debug, Deserialize)]
pub struct UserAdminForm {
    pub csrf_token: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub pin: String,
    #[serde(default)]
    pub is_active: String,
}

#[derive(Debug, Deserialize)]
pub struct PaymentMethodAdminForm {
    pub csrf_token: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub currency: String,
    #[serde(default)]
    pub sort_order: String,
    #[serde(default)]
    pub is_active: String,
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
pub struct OpenCashForm {
    pub csrf_token: String,
    #[serde(default)]
    pub opening_base: String,
    #[serde(default)]
    pub opening_secondary: String,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Deserialize)]
pub struct CashMovementForm {
    pub csrf_token: String,
    pub kind: String,
    pub currency: String,
    pub amount: String,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Deserialize)]
pub struct CloseCashForm {
    pub csrf_token: String,
    #[serde(default)]
    pub counted_base: String,
    #[serde(default)]
    pub counted_secondary: String,
    #[serde(default)]
    pub notes: String,
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

#[derive(Debug, Deserialize)]
pub struct CancelOrderForm {
    pub csrf_token: String,
    #[serde(default)]
    pub reason: String,
}

/// Sends the browser to the app or the login page.
/// Replaces JSON client errors with a small Spanish page when a browser is
/// navigating or submitting a form.
pub async fn html_error_page(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let accepts_html = request
        .headers()
        .get(header::ACCEPT)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.contains("text/html"));
    let back_href = request
        .headers()
        .get(header::REFERER)
        .and_then(|value| value.to_str().ok())
        .filter(|value| value.starts_with('/'))
        .unwrap_or("/app")
        .to_owned();

    let response = next.run(request).await;
    if !accepts_html || !response.status().is_client_error() {
        return response;
    }

    let status = response.status();
    let (title, message) = match status {
        StatusCode::UNAUTHORIZED => ("Sesión expirada", "Vuelve a iniciar sesión para continuar."),
        StatusCode::FORBIDDEN => (
            "Acción no permitida",
            "No tienes permiso para hacer esto o el formulario expiró. Recarga la página e inténtalo de nuevo.",
        ),
        StatusCode::NOT_FOUND => (
            "No encontrado",
            "El registro que buscas no existe o fue movido.",
        ),
        StatusCode::CONFLICT => (
            "Datos desactualizados",
            "Otro usuario cambió este registro. Actualiza la página e inténtalo de nuevo.",
        ),
        StatusCode::UNPROCESSABLE_ENTITY => (
            "Revisa los datos",
            "La acción no se pudo completar con los datos enviados. Revisa los montos y vuelve a intentarlo.",
        ),
        _ => (
            "No se pudo completar",
            "Ocurrió un problema al procesar la solicitud. Inténtalo de nuevo.",
        ),
    };

    let html = ErrorTemplate {
        status: status.as_u16(),
        title,
        message,
        back_href,
    }
    .render()
    .unwrap_or_else(|_| "No se pudo completar la solicitud.".to_owned());

    (status, Html(html)).into_response()
}

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

/// Formats the remaining lock for the Spanish login page.
fn retry_after_label(seconds: i64) -> String {
    if seconds <= 60 {
        "1 minuto".to_owned()
    } else if seconds < 3_600 {
        format!("{} minutos", (seconds + 59) / 60)
    } else {
        "1 hora".to_owned()
    }
}

/// Validates the PIN, creates a web session, and redirects to the app.
pub async fn login_submit(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let login = match auth::login(
        &state.pool,
        &form.user,
        &form.pin,
        SessionKind::Web,
        None,
        state.config.session_hours,
    )
    .await
    {
        Ok(Some(login)) => login,
        Ok(None) => {
            let html = LoginTemplate {
                error: "Usuario o PIN incorrecto.".to_owned(),
            }
            .render()?;

            return Ok((StatusCode::UNAUTHORIZED, Html(html)).into_response());
        }
        Err(AppError::TooManyAttempts {
            retry_after_seconds,
        }) => {
            let html = LoginTemplate {
                error: format!(
                    "Demasiados intentos. Espera {} e intenta de nuevo.",
                    retry_after_label(retry_after_seconds)
                ),
            }
            .render()?;
            let mut response = (StatusCode::TOO_MANY_REQUESTS, Html(html)).into_response();
            let value = HeaderValue::from_str(&retry_after_seconds.to_string())
                .unwrap_or_else(|_| HeaderValue::from_static("60"));
            response.headers_mut().insert(header::RETRY_AFTER, value);

            return Ok(response);
        }
        Err(error) => return Err(error),
    };

    let (_, device_cookie) = auth::resolve_web_device(&state.pool, &jar, &login.user).await?;
    let mut jar = jar.add(auth::session_cookie(login.token));
    if let Some(device_cookie) = device_cookie {
        jar = jar.add(device_cookie);
    }

    Ok((jar, Redirect::to("/app")).into_response())
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

    if !TABLE_ROLES.contains(&session.role.as_str()) {
        return forbidden_page();
    }

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

    let device_id = web_device(&state, &session).await?;
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

    if !TABLE_ROLES.contains(&session.role.as_str()) {
        return forbidden_page();
    }

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
    let device_id = web_device(&state, &session).await?;
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

    if !TABLE_ROLES.contains(&session.role.as_str()) {
        return forbidden_page();
    }

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
        active: order.active,
        order,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Returns the live status fragment of an order.
pub async fn order_status_fragment(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !TABLE_ROLES.contains(&session.role.as_str()) {
        return forbidden_page();
    }

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;
    let order = order_view(&state, &session, detail).await?;

    let html = OrderStatusTemplate { order }.render()?;

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
    let device_id = web_device(&state, &session).await?;

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
    let device_id = web_device(&state, &session).await?;

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
    let device_id = web_device(&state, &session).await?;

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
    let device_id = web_device(&state, &session).await?;

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
    let device_id = web_device(&state, &session).await?;
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
    let device_id = web_device(&state, &session).await?;

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
    let device_id = web_device(&state, &session).await?;
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

/// Cancels an open order with an audited reason.
pub async fn cancel_order(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
    Form(form): Form<CancelOrderForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?;
    let order_key = order_id.to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_key)
        .await?
        .ok_or(AppError::NotFound)?;
    if !matches!(detail.order.status.as_str(), "open" | "active" | "served") {
        return Err(AppError::Conflict(
            "the order cannot be cancelled".to_owned(),
        ));
    }
    let reason = form.reason.trim();
    if reason.is_empty() {
        return Err(AppError::Validation(
            "enter the cancellation reason".to_owned(),
        ));
    }
    if sale_for_order(&state.pool, &order_key).await?.is_some() {
        return Err(AppError::Conflict(
            "a billed order cannot be cancelled".to_owned(),
        ));
    }

    let branch_id = order_branch(&state, &session, &order_key).await?;
    let device_id = web_device(&state, &session).await?;
    let payload = json!({
        "reason": reason,
        "cancelled_at_ms": now_ms(),
        "cancelled_by": session.user_id,
    });
    append_order_operation(
        &state,
        &session,
        device_id,
        branch_id,
        OperationKind::OrderCancelled,
        order_id.into_uuid(),
        payload,
        "order_cancelled",
    )
    .await?;

    let back = if detail.order.order_type == "takeout" {
        "/app/takeout"
    } else {
        "/app/tables"
    };

    Ok(Redirect::to(back).into_response())
}

/// Renders the kitchen board.
pub async fn kitchen_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    if !KITCHEN_ROLES.contains(&session.role.as_str()) {
        return forbidden_page();
    }

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

    if !KITCHEN_ROLES.contains(&session.role.as_str()) {
        return Ok((StatusCode::FORBIDDEN, Html("Sin permiso.")).into_response());
    }

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

/// Renders the cashier dashboard: open cash session and billable orders.
pub async fn cashier_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    require_roles(&session, &["admin", "cashier"])?;

    let device_id = web_device(&state, &session).await?;
    let branch_id = cash_branch(&state, &session, &device_id).await?;
    let cash = load_cash_session(&state, &device_id, &branch_id).await?;
    let currency = base_currency(&state.pool, &session.business_id).await?;
    let orders = projection::open_orders(&state.pool, &session.business_id)
        .await?
        .iter()
        .map(|order| order_summary_view(order, &currency))
        .collect();

    let html = CashierTemplate {
        common: page_context(&session),
        active: "cashier",
        session: cash,
        orders,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Renders the billing form for one order.
pub async fn bill_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    require_roles(&session, &["admin", "cashier"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;

    if detail.order.status == "billed"
        && let Some(sale_id) = sale_for_order(&state.pool, &order_id).await?
    {
        return Ok(Redirect::to(&format!("/app/sales/{sale_id}")).into_response());
    }
    if detail.order.status == "cancelled" {
        return Ok(Redirect::to(&format!("/app/orders/{order_id}")).into_response());
    }

    let base = base_currency(&state.pool, &session.business_id).await?;
    let secondary = billing::secondary_currency(&base).to_owned();
    let tax = tax_config(&state.pool, &session.business_id).await?;
    let total_minor = detail.order.total_minor;
    let (tax_minor, tax_name, tax_rate_label, tax_enabled) = match tax {
        Some((name, rate, true)) => {
            let amount = billing::included_tax_minor(total_minor, rate)?;
            (amount, name, format_rate_label(rate), true)
        }
        Some((name, rate, false)) => (0, name, format_rate_label(rate), false),
        None => (0, "IVA".to_owned(), "0%".to_owned(), false),
    };
    let subtotal_minor = total_minor - tax_minor;
    let exchange_rate_micros = exchange_rate(&state.pool, &session.business_id).await?;
    let device_id = web_device(&state, &session).await?;
    let branch_id = cash_branch(&state, &session, &device_id).await?;
    let has_cash_session = open_cash_session_row(&state.pool, &branch_id, &device_id)
        .await?
        .is_some();

    let lines = detail
        .lines
        .iter()
        .filter(|line| line.status != "voided")
        .map(|line| BillLineView {
            description: line.description.clone(),
            quantity: line.quantity,
            total: format_price(line.line_total_minor, &line.currency),
            notes: line.notes.clone().unwrap_or_default(),
        })
        .collect();

    let html = BillTemplate {
        common: page_context(&session),
        active: "cashier",
        order: BillOrderView {
            id: detail.order.id.clone(),
            label: order_label(
                &detail.order.order_type,
                detail.order.table_name.as_deref(),
                detail.order.customer_name.as_deref(),
                &detail.order.display_code,
            ),
            display_code: detail.order.display_code.clone(),
            order_type_label: order_type_label(&detail.order.order_type),
            opened_at: format_local_time(detail.order.opened_at_ms),
            lines,
        },
        methods: load_payment_methods(&state, &session).await?,
        base_currency: base.clone(),
        secondary_currency: secondary,
        exchange_rate: if exchange_rate_micros > 0 {
            billing::format_rate_micros(exchange_rate_micros)
        } else {
            String::new()
        },
        tax_enabled,
        tax_name,
        tax_rate_label,
        subtotal: format_price(subtotal_minor, &base),
        tax: format_price(tax_minor, &base),
        total: format_price(total_minor, &base),
        has_cash_session,
    }
    .render()?;

    Ok(Html(html).into_response())
}

struct PreparedPayment {
    method_id: String,
    method_kind: String,
    amount_minor: i64,
    currency: String,
    base_amount_minor: i64,
    exchange_rate_micros: Option<i64>,
    tendered_minor: i64,
    change_minor: i64,
    reference: Option<String>,
}

/// Bills an order with one or more payments and writes the sale, the order
/// transition, and the cash movements in one transaction.
pub async fn bill_order(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(order_id): Path<String>,
    body: Bytes,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    let form = parse_form(&body);
    let Some(csrf_token) = first_value(&form, "csrf_token") else {
        return Err(AppError::Forbidden);
    };
    if !auth::csrf_matches(&session, csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier"])?;

    let order_id = order_id
        .parse::<OrderId>()
        .map_err(|_| AppError::Validation("invalid order id".to_owned()))?
        .to_string();
    let detail = projection::order_detail(&state.pool, &session.business_id, &order_id)
        .await?
        .ok_or(AppError::NotFound)?;

    if detail.order.status == "billed" {
        if let Some(sale_id) = sale_for_order(&state.pool, &order_id).await? {
            return Ok(Redirect::to(&format!("/app/sales/{sale_id}")).into_response());
        }
        return Err(AppError::Validation(
            "the order is already billed".to_owned(),
        ));
    }
    if !matches!(detail.order.status.as_str(), "open" | "active" | "served") {
        return Err(AppError::Validation(
            "the order cannot be billed".to_owned(),
        ));
    }

    let total_minor = detail.order.total_minor;
    if total_minor <= 0 {
        return Err(AppError::Validation(
            "the order total must be greater than zero".to_owned(),
        ));
    }

    let base = base_currency(&state.pool, &session.business_id).await?;
    let secondary = billing::secondary_currency(&base).to_owned();
    let tax = tax_config(&state.pool, &session.business_id).await?;
    let (tax_minor, tax_name, tax_rate_basis_points) = match tax {
        Some((name, rate, true)) => (
            billing::included_tax_minor(total_minor, rate)?,
            Some(name),
            Some(rate),
        ),
        _ => (0, None, None),
    };
    let subtotal_minor = total_minor - tax_minor;

    let method_values = all_values(&form, "method_id");
    let currency_values = all_values(&form, "currency");
    let amount_values = all_values(&form, "amount");
    let reference_values = all_values(&form, "reference");

    let mut rows = Vec::new();
    for (index, method_value) in method_values.iter().enumerate() {
        let method_id = method_value.trim();
        let amount_text = amount_values
            .get(index)
            .map(|value| value.trim())
            .unwrap_or("");
        if method_id.is_empty() && amount_text.is_empty() {
            continue;
        }
        if method_id.is_empty() || amount_text.is_empty() {
            return Err(AppError::Validation(
                "each payment needs a method and an amount".to_owned(),
            ));
        }
        rows.push(index);
    }
    if rows.is_empty() {
        return Err(AppError::Validation("add at least one payment".to_owned()));
    }

    let rate_input = first_value(&form, "exchange_rate").unwrap_or("");
    let stored_rate = exchange_rate(&state.pool, &session.business_id).await?;
    let entered_rate = if rate_input.trim().is_empty() {
        stored_rate
    } else {
        billing::parse_rate_micros(rate_input)?
    };

    let mut payments = Vec::with_capacity(rows.len());
    let mut remaining = total_minor;

    for (position, &index) in rows.iter().enumerate() {
        let is_last = position + 1 == rows.len();
        let method_id = method_values[index].trim().to_owned();
        let method = payment_method(&state.pool, &session.business_id, &method_id).await?;
        let Some((_name, kind, method_currency, true)) = method else {
            return Err(AppError::Validation(
                "the payment method is not available".to_owned(),
            ));
        };

        let currency = currency_values
            .get(index)
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or_else(|| method_currency.clone())
            .unwrap_or_else(|| base.clone());
        if currency != base && currency != secondary {
            return Err(AppError::Validation(
                "the payment currency is not supported by this business".to_owned(),
            ));
        }
        if let Some(method_currency) = &method_currency
            && method_currency != &currency
        {
            return Err(AppError::Validation(
                "the payment method does not accept that currency".to_owned(),
            ));
        }

        let amount_minor = billing::parse_amount_minor(amount_values[index])?;
        if amount_minor <= 0 {
            return Err(AppError::Validation(
                "payment amounts must be greater than zero".to_owned(),
            ));
        }
        let exchange_rate_micros = if currency == base {
            None
        } else {
            Some(entered_rate)
        };
        let tendered_base = billing::to_base_minor(
            amount_minor,
            &currency,
            &base,
            exchange_rate_micros.unwrap_or(0),
        )?;
        if tendered_base <= 0 {
            return Err(AppError::Validation(
                "the payment amount is too small for the exchange rate".to_owned(),
            ));
        }

        let (base_amount_minor, applied_amount_minor) = if is_last {
            if tendered_base < remaining {
                return Err(AppError::Validation(
                    "the payments do not cover the sale total".to_owned(),
                ));
            }
            let applied = billing::amount_covering_base(
                remaining,
                &currency,
                &base,
                exchange_rate_micros.unwrap_or(0),
            )?;
            if applied > amount_minor {
                return Err(AppError::Validation(
                    "the payment amount is too small for the exchange rate".to_owned(),
                ));
            }
            (remaining, applied)
        } else {
            if tendered_base >= remaining {
                return Err(AppError::Validation(
                    "only the last payment can exceed the remaining balance".to_owned(),
                ));
            }
            (tendered_base, amount_minor)
        };

        remaining -= base_amount_minor;
        payments.push(PreparedPayment {
            method_id,
            method_kind: kind,
            amount_minor: applied_amount_minor,
            currency,
            base_amount_minor,
            exchange_rate_micros,
            tendered_minor: amount_minor,
            change_minor: amount_minor - applied_amount_minor,
            reference: reference_values
                .get(index)
                .map(|value| value.trim())
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        });
    }

    if remaining != 0 {
        return Err(AppError::Validation(
            "the payments do not add up to the sale total".to_owned(),
        ));
    }

    let device_id = web_device(&state, &session).await?;
    let branch_id = order_branch(&state, &session, &order_id).await?;
    let cash_session = if payments.iter().any(|payment| payment.method_kind == "cash") {
        let Some(cash_session) = open_cash_session_row(&state.pool, &branch_id, &device_id).await?
        else {
            return Err(AppError::Validation(
                "cash payments require an open cash session".to_owned(),
            ));
        };
        for payment in &payments {
            if payment.method_kind == "cash"
                && payment.currency != cash_session.currency
                && payment.currency != cash_session.secondary_currency
            {
                return Err(AppError::Validation(
                    "the cash session does not hold that currency".to_owned(),
                ));
            }
        }
        Some(cash_session)
    } else {
        None
    };

    let _billing_guard = state.billing_lock.lock().await;
    let now = now_ms();
    let number = next_sale_number(&state.pool, &branch_id.to_string()).await?;
    let sale_id = SaleId::new();
    let sale_lines = detail
        .lines
        .iter()
        .filter(|line| line.status != "voided")
        .map(|line| {
            json!({
                "line_id": Uuid::now_v7().to_string(),
                "order_line_id": line.id,
                "description": line.description,
                "quantity": line.quantity,
                "unit_price_minor": line.unit_price_minor,
                "line_total_minor": line.line_total_minor,
                "currency": line.currency,
            })
        })
        .collect::<Vec<_>>();
    let payment_payloads = payments
        .iter()
        .map(|payment| {
            json!({
                "payment_id": PaymentId::new().to_string(),
                "method_id": payment.method_id,
                "amount_minor": payment.amount_minor,
                "currency": payment.currency,
                "base_amount_minor": payment.base_amount_minor,
                "exchange_rate_micros": payment.exchange_rate_micros,
                "tendered_minor": payment.tendered_minor,
                "change_minor": payment.change_minor,
                "reference": payment.reference,
                "created_by": session.user_id,
                "created_at_ms": now,
            })
        })
        .collect::<Vec<_>>();

    let sale_operation = Operation::new(
        device_id,
        Some(branch_id),
        OperationKind::SaleCreated,
        sale_id.into_uuid(),
        json!({
            "order_id": order_id,
            "number": number,
            "subtotal_minor": subtotal_minor,
            "tax_minor": tax_minor,
            "total_minor": total_minor,
            "currency": base,
            "tax_name": tax_name,
            "tax_rate_basis_points": tax_rate_basis_points,
            "cash_session_id": cash_session.as_ref().map(|session| session.id.as_str()),
            "created_by": session.user_id,
            "created_at_ms": now,
            "lines": sale_lines,
            "payments": payment_payloads,
        }),
        now,
    );

    let billed_operation = Operation::new(
        device_id,
        Some(branch_id),
        OperationKind::OrderBilled,
        detail
            .order
            .id
            .parse::<OrderId>()
            .map_err(|_| AppError::CorruptData("invalid order id".to_owned()))?
            .into_uuid(),
        json!({
            "sale_id": sale_id.to_string(),
            "created_by": session.user_id,
            "created_at_ms": now,
        }),
        now,
    );

    let mut operations = vec![
        WebOperation {
            operation: sale_operation,
            action: "sale_created".to_owned(),
            details: json!({
                "order_id": detail.order.id,
                "sale_id": sale_id.to_string(),
                "total_minor": total_minor,
            }),
        },
        WebOperation {
            operation: billed_operation,
            action: "order_billed".to_owned(),
            details: json!({
                "order_id": detail.order.id,
                "sale_id": sale_id.to_string(),
            }),
        },
    ];

    if let Some(cash_session) = &cash_session {
        for payment in &payments {
            if payment.method_kind != "cash" {
                continue;
            }
            operations.push(WebOperation {
                operation: Operation::new(
                    device_id,
                    Some(branch_id),
                    OperationKind::CashMovementRecorded,
                    CashMovementId::new().into_uuid(),
                    json!({
                        "cash_session_id": cash_session.id,
                        "kind": "sale",
                        "amount_minor": payment.amount_minor,
                        "currency": payment.currency,
                        "reference_type": "sale",
                        "reference_id": sale_id.to_string(),
                        "created_by": session.user_id,
                        "created_at_ms": now,
                    }),
                    now,
                ),
                action: "cash_movement_recorded".to_owned(),
                details: json!({
                    "cash_session_id": cash_session.id,
                    "sale_id": sale_id.to_string(),
                    "amount_minor": payment.amount_minor,
                }),
            });
        }
    }

    sync::append_web_operations(
        &state.pool,
        operations,
        &session.user_id,
        &session.business_id,
        Some(&branch_id.to_string()),
    )
    .await?;

    Ok(Redirect::to(&format!("/app/sales/{sale_id}")).into_response())
}

/// Renders a printable receipt for one sale.
pub async fn receipt_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Path(sale_id): Path<String>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    require_roles(&session, &["admin", "cashier"])?;

    let sale_id = sale_id
        .parse::<SaleId>()
        .map_err(|_| AppError::Validation("invalid sale id".to_owned()))?
        .to_string();
    let sale = load_receipt(&state, &session, &sale_id).await?;

    let html = ReceiptTemplate {
        common: page_context(&session),
        active: "cashier",
        sale,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Renders the cash session page with movements and closing form.
pub async fn cash_page(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    require_roles(&session, &["admin", "cashier"])?;

    let device_id = web_device(&state, &session).await?;
    let branch_id = cash_branch(&state, &session, &device_id).await?;
    let cash = load_cash_session(&state, &device_id, &branch_id).await?;
    let base = base_currency(&state.pool, &session.business_id).await?;
    let secondary = billing::secondary_currency(&base).to_owned();

    let html = CashTemplate {
        common: page_context(&session),
        active: "cash",
        session: cash,
        base_currency: base,
        secondary_currency: secondary,
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Opens a cash session for this browser.
pub async fn open_cash(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<OpenCashForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier"])?;

    let device_id = web_device(&state, &session).await?;
    let branch_id = cash_branch(&state, &session, &device_id).await?;
    if open_cash_session_row(&state.pool, &branch_id, &device_id)
        .await?
        .is_some()
    {
        return Err(AppError::Validation(
            "this register already has an open cash session".to_owned(),
        ));
    }

    let base = base_currency(&state.pool, &session.business_id).await?;
    let secondary = billing::secondary_currency(&base).to_owned();
    let opening_base = billing::parse_amount_minor(&form.opening_base)?;
    let opening_secondary = billing::parse_amount_minor(&form.opening_secondary)?;
    let now = now_ms();
    let cash_session_id = CashSessionId::new();
    let operation = Operation::new(
        device_id,
        Some(branch_id),
        OperationKind::CashSessionOpened,
        cash_session_id.into_uuid(),
        json!({
            "cash_session_id": cash_session_id.to_string(),
            "branch_id": branch_id.to_string(),
            "device_id": device_id.to_string(),
            "opened_by": session.user_id,
            "opening_amount_minor": opening_base,
            "currency": base,
            "secondary_currency": secondary,
            "secondary_opening_amount_minor": opening_secondary,
            "opened_at_ms": now,
            "notes": clean_optional(&form.notes),
        }),
        now,
    );

    sync::append_web_operation(
        &state.pool,
        &operation,
        &session.user_id,
        &session.business_id,
        Some(&branch_id.to_string()),
        "cash_session_opened",
        json!({ "cash_session_id": cash_session_id.to_string() }),
    )
    .await?;

    Ok(Redirect::to("/app/cash").into_response())
}

/// Records a manual cash-in or cash-out movement.
pub async fn record_cash(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CashMovementForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier"])?;

    let device_id = web_device(&state, &session).await?;
    let branch_id = cash_branch(&state, &session, &device_id).await?;
    let Some(cash_session) = open_cash_session_row(&state.pool, &branch_id, &device_id).await?
    else {
        return Err(AppError::Validation(
            "open the cash session first".to_owned(),
        ));
    };

    if !matches!(form.kind.as_str(), "in" | "out") {
        return Err(AppError::Validation("invalid cash movement".to_owned()));
    }
    let base = base_currency(&state.pool, &session.business_id).await?;
    let secondary = billing::secondary_currency(&base).to_owned();
    if form.currency != base && form.currency != secondary {
        return Err(AppError::Validation(
            "the cash session does not hold that currency".to_owned(),
        ));
    }
    let amount_minor = billing::parse_amount_minor(&form.amount)?;
    if amount_minor <= 0 {
        return Err(AppError::Validation(
            "cash movement amounts must be greater than zero".to_owned(),
        ));
    }

    let now = now_ms();
    let movement_id = CashMovementId::new();
    let operation = Operation::new(
        device_id,
        Some(branch_id),
        OperationKind::CashMovementRecorded,
        movement_id.into_uuid(),
        json!({
            "cash_session_id": cash_session.id,
            "kind": form.kind,
            "amount_minor": amount_minor,
            "currency": form.currency,
            "reason": clean_optional(&form.reason),
            "created_by": session.user_id,
            "created_at_ms": now,
        }),
        now,
    );

    sync::append_web_operation(
        &state.pool,
        &operation,
        &session.user_id,
        &session.business_id,
        Some(&branch_id.to_string()),
        "cash_movement_recorded",
        json!({
            "cash_session_id": cash_session.id,
            "kind": form.kind,
            "amount_minor": amount_minor,
        }),
    )
    .await?;

    Ok(Redirect::to("/app/cash").into_response())
}

/// Closes the open cash session with the counted amounts.
pub async fn close_cash(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CloseCashForm>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }
    require_roles(&session, &["admin", "cashier"])?;

    let device_id = web_device(&state, &session).await?;
    let branch_id = cash_branch(&state, &session, &device_id).await?;
    let Some(cash_session) = open_cash_session_row(&state.pool, &branch_id, &device_id).await?
    else {
        return Err(AppError::Validation(
            "there is no open cash session".to_owned(),
        ));
    };

    let base = base_currency(&state.pool, &session.business_id).await?;
    let secondary = billing::secondary_currency(&base).to_owned();
    let (expected_base, expected_secondary) =
        cash_expected(&state.pool, &cash_session.id, &base, &secondary).await?;
    let counted_base = billing::parse_amount_minor(&form.counted_base)?;
    let counted_secondary = billing::parse_amount_minor(&form.counted_secondary)?;
    let now = now_ms();
    let operation = Operation::new(
        device_id,
        Some(branch_id),
        OperationKind::CashSessionClosed,
        cash_session
            .id
            .parse::<CashSessionId>()
            .map_err(|_| AppError::CorruptData("invalid cash session id".to_owned()))?
            .into_uuid(),
        json!({
            "expected_amount_minor": expected_base,
            "counted_amount_minor": counted_base,
            "difference_minor": counted_base - expected_base,
            "secondary_expected_amount_minor": expected_secondary,
            "secondary_counted_amount_minor": counted_secondary,
            "secondary_difference_minor": counted_secondary - expected_secondary,
            "closed_by": session.user_id,
            "closed_at_ms": now,
            "notes": clean_optional(&form.notes),
        }),
        now,
    );

    sync::append_web_operation(
        &state.pool,
        &operation,
        &session.user_id,
        &session.business_id,
        Some(&branch_id.to_string()),
        "cash_session_closed",
        json!({
            "cash_session_id": cash_session.id,
            "expected_amount_minor": expected_base,
            "counted_amount_minor": counted_base,
        }),
    )
    .await?;

    Ok(Redirect::to("/app/cash").into_response())
}

/// Renders the daily sales report.
pub async fn reports_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<ReportQuery>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    require_roles(&session, &["admin", "cashier"])?;

    let date = report_date(&query)?;
    let report = load_report(&state, &session, &date).await?;
    let base = report.base_currency.clone();
    let tax = tax_config(&state.pool, &session.business_id).await?;
    let tax_label = match tax {
        Some((name, rate, true)) => {
            format!("{name} incluido ({})", format_rate_label(rate))
        }
        _ => "IVA".to_owned(),
    };
    let average = if report.sale_count > 0 {
        report.total_minor / report.sale_count
    } else {
        0
    };

    let html = ReportsTemplate {
        common: page_context(&session),
        active: "reports",
        date: date.clone(),
        base_currency: base.clone(),
        sale_count: report.sale_count,
        subtotal: format_price(report.subtotal_minor, &base),
        tax: format_price(report.tax_minor, &base),
        tax_label,
        total: format_price(report.total_minor, &base),
        average: format_price(average, &base),
        has_tax: report.tax_minor > 0,
        payments: report
            .payments
            .iter()
            .map(|payment| ReportPaymentView {
                method: payment.method.clone(),
                kind_label: payment_kind_label(&payment.kind),
                amount: format_price(payment.amount_minor, &payment.currency),
                base_amount: format_price(payment.base_amount_minor, &base),
                count: payment.count,
            })
            .collect(),
        top_items: report
            .top_items
            .iter()
            .map(|item| ReportItemView {
                description: item.description.clone(),
                quantity: item.quantity,
                total: format_price(item.total_minor, &item.currency),
            })
            .collect(),
        sales: report
            .sales
            .iter()
            .map(|sale| ReportSaleView {
                id: sale.id.clone(),
                number: sale.number.clone(),
                time: format_local_time(sale.created_at_ms),
                label: sale.label.clone(),
                type_label: order_type_label(&sale.order_type),
                total: format_price(sale.total_minor, &sale.currency),
                status_label: if sale.status == "cancelled" {
                    "Anulada"
                } else {
                    "Pagada"
                },
            })
            .collect(),
        cash_sessions: report
            .cash_sessions
            .iter()
            .map(|cash| ReportCashView {
                device: cash.device.clone(),
                opened_at: format_local_datetime(cash.opened_at_ms),
                closed_at: cash
                    .closed_at_ms
                    .map(format_local_datetime)
                    .unwrap_or_else(|| "—".to_owned()),
                status_label: if cash.status == "closed" {
                    "Cerrada"
                } else {
                    "Abierta"
                },
                base_currency: cash.base_currency.clone(),
                secondary_currency: cash.secondary_currency.clone(),
                opening_base: format_price(cash.opening_base, &cash.base_currency),
                expected_base: format_price(cash.expected_base.unwrap_or(0), &cash.base_currency),
                counted_base: format_price(cash.counted_base.unwrap_or(0), &cash.base_currency),
                difference_base: format_price(
                    cash.difference_base.unwrap_or(0),
                    &cash.base_currency,
                ),
                opening_secondary: format_price(cash.opening_secondary, &cash.secondary_currency),
                expected_secondary: format_price(
                    cash.expected_secondary.unwrap_or(0),
                    &cash.secondary_currency,
                ),
                counted_secondary: format_price(
                    cash.counted_secondary.unwrap_or(0),
                    &cash.secondary_currency,
                ),
                difference_secondary: format_price(
                    cash.difference_secondary.unwrap_or(0),
                    &cash.secondary_currency,
                ),
            })
            .collect(),
    }
    .render()?;

    Ok(Html(html).into_response())
}

/// Downloads the sales of one day as CSV.
pub async fn reports_csv(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<ReportQuery>,
) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    require_roles(&session, &["admin", "cashier"])?;

    let date = report_date(&query)?;
    let report = load_report(&state, &session, &date).await?;
    let mut csv =
        String::from("\u{feff}numero,fecha,codigo,tipo,estado,subtotal,iva,total,moneda,cajero\n");
    for sale in &report.sales {
        let fields = [
            sale.number.clone(),
            format_local_datetime(sale.created_at_ms),
            sale.display_code.clone(),
            order_type_label(&sale.order_type).to_owned(),
            if sale.status == "cancelled" {
                "Anulada".to_owned()
            } else {
                "Pagada".to_owned()
            },
            csv_amount(sale.subtotal_minor),
            csv_amount(sale.tax_minor),
            csv_amount(sale.total_minor),
            sale.currency.clone(),
            sale.cashier.clone(),
        ];
        csv.push_str(
            &fields
                .iter()
                .map(|field| csv_field(field))
                .collect::<Vec<_>>()
                .join(","),
        );
        csv.push('\n');
    }

    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/csv; charset=utf-8"),
        ),
        (
            header::CONTENT_DISPOSITION,
            HeaderValue::try_from(format!("attachment; filename=\"ventas-{date}.csv\""))
                .map_err(|_| AppError::Internal("invalid CSV filename".to_owned()))?,
        ),
    ];

    Ok((headers, csv).into_response())
}

fn report_date(query: &ReportQuery) -> Result<String, AppError> {
    let date = query.date.trim();
    let date = if date.is_empty() {
        today_local_date()
    } else {
        date.to_owned()
    };

    if local_day_bounds(&date).is_none() {
        return Err(AppError::Validation(
            "the report date must use YYYY-MM-DD".to_owned(),
        ));
    }

    Ok(date)
}

fn csv_amount(minor: i64) -> String {
    let sign = if minor < 0 { "-" } else { "" };
    let absolute = minor.unsigned_abs();
    format!("{sign}{}.{:02}", absolute / 100, absolute % 100)
}

fn csv_field(value: &str) -> String {
    if value
        .chars()
        .any(|character| matches!(character, ',' | '"' | '\n' | '\r'))
    {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

struct ReportRows {
    base_currency: String,
    sale_count: i64,
    subtotal_minor: i64,
    tax_minor: i64,
    total_minor: i64,
    payments: Vec<ReportPaymentRow>,
    top_items: Vec<ReportItemRow>,
    sales: Vec<ReportSaleRow>,
    cash_sessions: Vec<ReportCashRow>,
}

struct ReportPaymentRow {
    method: String,
    kind: String,
    currency: String,
    amount_minor: i64,
    base_amount_minor: i64,
    count: i64,
}

struct ReportItemRow {
    description: String,
    quantity: i64,
    total_minor: i64,
    currency: String,
}

struct ReportSaleRow {
    id: String,
    number: String,
    created_at_ms: i64,
    display_code: String,
    label: String,
    order_type: String,
    status: String,
    subtotal_minor: i64,
    tax_minor: i64,
    total_minor: i64,
    currency: String,
    cashier: String,
}

struct ReportCashRow {
    device: String,
    opened_at_ms: i64,
    closed_at_ms: Option<i64>,
    status: String,
    base_currency: String,
    secondary_currency: String,
    opening_base: i64,
    expected_base: Option<i64>,
    counted_base: Option<i64>,
    difference_base: Option<i64>,
    opening_secondary: i64,
    expected_secondary: Option<i64>,
    counted_secondary: Option<i64>,
    difference_secondary: Option<i64>,
}

async fn load_report(
    state: &AppState,
    session: &SessionUser,
    date: &str,
) -> Result<ReportRows, AppError> {
    use sqlx::Row;

    let (start, end) = local_day_bounds(date)
        .ok_or_else(|| AppError::Validation("invalid report date".to_owned()))?;
    let base_currency = base_currency(&state.pool, &session.business_id).await?;

    let summary = sqlx::query(
        "SELECT
            COUNT(*) AS sale_count,
            COALESCE(SUM(subtotal_minor), 0) AS subtotal_minor,
            COALESCE(SUM(tax_minor), 0) AS tax_minor,
            COALESCE(SUM(total_minor), 0) AS total_minor
         FROM sale
         WHERE business_id = ?1 AND status = 'completed'
           AND created_at_ms >= ?2 AND created_at_ms < ?3",
    )
    .bind(&session.business_id)
    .bind(start)
    .bind(end)
    .fetch_one(&state.pool)
    .await?;

    let payment_rows = sqlx::query(
        "SELECT
            COALESCE(m.name, 'Otro') AS method,
            COALESCE(m.kind, '') AS kind,
            p.currency,
            SUM(p.amount_minor) AS amount_minor,
            SUM(p.base_amount_minor) AS base_amount_minor,
            COUNT(*) AS payment_count
         FROM payment p
         JOIN sale s ON s.id = p.sale_id
         LEFT JOIN payment_method m ON m.id = p.method_id
         WHERE s.business_id = ?1 AND s.status = 'completed'
           AND s.created_at_ms >= ?2 AND s.created_at_ms < ?3
         GROUP BY m.id, p.currency
         ORDER BY method, p.currency",
    )
    .bind(&session.business_id)
    .bind(start)
    .bind(end)
    .fetch_all(&state.pool)
    .await?;

    let item_rows = sqlx::query(
        "SELECT
            sl.description,
            sl.currency,
            SUM(sl.quantity) AS quantity,
            SUM(sl.line_total_minor) AS total_minor
         FROM sale_line sl
         JOIN sale s ON s.id = sl.sale_id
         WHERE s.business_id = ?1 AND s.status = 'completed'
           AND s.created_at_ms >= ?2 AND s.created_at_ms < ?3
         GROUP BY sl.description, sl.currency
         ORDER BY quantity DESC, total_minor DESC
         LIMIT 10",
    )
    .bind(&session.business_id)
    .bind(start)
    .bind(end)
    .fetch_all(&state.pool)
    .await?;

    let sale_rows = sqlx::query(
        "SELECT
            s.id, s.number, s.created_at_ms, s.status, s.subtotal_minor,
            s.tax_minor, s.total_minor, s.currency,
            COALESCE(u.display_name, '—') AS cashier,
            o.order_type, o.display_code, o.customer_name,
            t.name AS table_name
         FROM sale s
         JOIN restaurant_order o ON o.id = s.order_id
         LEFT JOIN dining_table t ON t.id = o.table_id
         LEFT JOIN app_user u ON u.id = s.created_by
         WHERE s.business_id = ?1 AND s.status = 'completed'
           AND s.created_at_ms >= ?2 AND s.created_at_ms < ?3
         ORDER BY s.created_at_ms DESC",
    )
    .bind(&session.business_id)
    .bind(start)
    .bind(end)
    .fetch_all(&state.pool)
    .await?;

    let cash_rows = sqlx::query(
        "SELECT
            cs.opened_at_ms, cs.closed_at_ms, cs.status,
            cs.currency, cs.secondary_currency,
            cs.opening_amount_minor, cs.secondary_opening_amount_minor,
            cs.expected_amount_minor, cs.counted_amount_minor,
            cs.difference_minor, cs.secondary_expected_amount_minor,
            cs.secondary_counted_amount_minor, cs.secondary_difference_minor,
            COALESCE(d.name, 'Caja') AS device
         FROM cash_session cs
         LEFT JOIN device d ON d.id = cs.device_id
         WHERE cs.business_id = ?1
           AND cs.opened_at_ms < ?3
           AND (cs.closed_at_ms IS NULL OR cs.closed_at_ms >= ?2)
         ORDER BY cs.opened_at_ms DESC",
    )
    .bind(&session.business_id)
    .bind(start)
    .bind(end)
    .fetch_all(&state.pool)
    .await?;

    let payments = payment_rows
        .iter()
        .map(|row| {
            Ok(ReportPaymentRow {
                method: row.try_get("method")?,
                kind: row.try_get("kind")?,
                currency: row.try_get("currency")?,
                amount_minor: row.try_get("amount_minor")?,
                base_amount_minor: row.try_get("base_amount_minor")?,
                count: row.try_get("payment_count")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let top_items = item_rows
        .iter()
        .map(|row| {
            Ok(ReportItemRow {
                description: row.try_get("description")?,
                quantity: row.try_get("quantity")?,
                total_minor: row.try_get("total_minor")?,
                currency: row.try_get("currency")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let sales = sale_rows
        .iter()
        .map(|row| {
            let order_type: String = row.try_get("order_type")?;
            let table_name: Option<String> = row.try_get("table_name")?;
            let customer_name: Option<String> = row.try_get("customer_name")?;
            let display_code: String = row.try_get("display_code")?;
            Ok(ReportSaleRow {
                id: row.try_get("id")?,
                number: row.try_get("number")?,
                created_at_ms: row.try_get("created_at_ms")?,
                display_code: display_code.clone(),
                label: order_label(
                    &order_type,
                    table_name.as_deref(),
                    customer_name.as_deref(),
                    &display_code,
                ),
                order_type,
                status: row.try_get("status")?,
                subtotal_minor: row.try_get("subtotal_minor")?,
                tax_minor: row.try_get("tax_minor")?,
                total_minor: row.try_get("total_minor")?,
                currency: row.try_get("currency")?,
                cashier: row.try_get("cashier")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let cash_sessions = cash_rows
        .iter()
        .map(|row| {
            Ok(ReportCashRow {
                device: row.try_get("device")?,
                opened_at_ms: row.try_get("opened_at_ms")?,
                closed_at_ms: row.try_get("closed_at_ms")?,
                status: row.try_get("status")?,
                base_currency: row.try_get("currency")?,
                secondary_currency: row.try_get("secondary_currency")?,
                opening_base: row.try_get("opening_amount_minor")?,
                expected_base: row.try_get("expected_amount_minor")?,
                counted_base: row.try_get("counted_amount_minor")?,
                difference_base: row.try_get("difference_minor")?,
                opening_secondary: row.try_get("secondary_opening_amount_minor")?,
                expected_secondary: row.try_get("secondary_expected_amount_minor")?,
                counted_secondary: row.try_get("secondary_counted_amount_minor")?,
                difference_secondary: row.try_get("secondary_difference_minor")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    Ok(ReportRows {
        base_currency,
        sale_count: summary.try_get("sale_count")?,
        subtotal_minor: summary.try_get("subtotal_minor")?,
        tax_minor: summary.try_get("tax_minor")?,
        total_minor: summary.try_get("total_minor")?,
        payments,
        top_items,
        sales,
        cash_sessions,
    })
}

/// Renders the administration page.
pub async fn admin_page(
    State(state): State<AppState>,
    jar: CookieJar,
    Query(query): Query<AdminQuery>,
) -> Result<Response, AppError> {
    let Some(session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    use sqlx::Row;
    let business = sqlx::query(
        "SELECT name, legal_name, tax_id, base_currency
         FROM business WHERE id = ?1",
    )
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;
    let base_currency: String = business.try_get("base_currency")?;
    let exchange_rate_micros = exchange_rate(&state.pool, &session.business_id).await?;
    let tax = tax_config(&state.pool, &session.business_id).await?;
    let (tax_name, tax_rate_basis_points, tax_enabled) =
        tax.unwrap_or_else(|| ("IVA".to_owned(), 0, false));

    let category_rows = sqlx::query(
        "SELECT id, name, sort_order, is_active
         FROM menu_category
         WHERE business_id = ?1
         ORDER BY sort_order, name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;
    let categories = category_rows
        .iter()
        .map(|row| {
            Ok(AdminCategoryView {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                sort_order: row.try_get("sort_order")?,
                is_active: row.try_get("is_active")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let item_rows = sqlx::query(
        "SELECT
            i.id, i.name, COALESCE(i.description, '') AS description,
            COALESCE(c.name, 'Sin categoría') AS category,
            i.price_minor, i.currency, i.is_active, i.is_available
         FROM menu_item i
         LEFT JOIN menu_category c ON c.id = i.category_id
         WHERE i.business_id = ?1
         ORDER BY c.sort_order, c.name, i.sort_order, i.name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;
    let items = item_rows
        .iter()
        .map(|row| {
            let currency: String = row.try_get("currency")?;
            Ok(AdminItemView {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                category: row.try_get("category")?,
                price: format_price(row.try_get("price_minor")?, &currency),
                is_active: row.try_get("is_active")?,
                is_available: row.try_get("is_available")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let table_rows = sqlx::query(
        "SELECT
            t.id, t.name, COALESCE(t.zone, '') AS zone, t.is_active,
            EXISTS (
                SELECT 1 FROM restaurant_order o
                WHERE o.table_id = t.id
                  AND o.status IN ('open', 'active', 'served')
            ) AS occupied
         FROM dining_table t
         WHERE t.business_id = ?1
         ORDER BY t.zone, t.name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;
    let tables = table_rows
        .iter()
        .map(|row| {
            Ok(AdminTableView {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                zone: row.try_get("zone")?,
                is_active: row.try_get("is_active")?,
                occupied: row.try_get("occupied")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let user_rows = sqlx::query(
        "SELECT id, display_name, role, is_active, pin_hash IS NOT NULL AS has_pin
         FROM app_user
         WHERE business_id = ?1
         ORDER BY display_name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;
    let users = user_rows
        .iter()
        .map(|row| {
            let role: String = row.try_get("role")?;
            Ok(AdminUserView {
                id: row.try_get("id")?,
                name: row.try_get("display_name")?,
                role_label: auth::role_label(&role).to_owned(),
                is_active: row.try_get("is_active")?,
                has_pin: row.try_get("has_pin")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let method_rows = sqlx::query(
        "SELECT id, name, kind, COALESCE(currency, '') AS currency,
                sort_order, is_active
         FROM payment_method
         WHERE business_id = ?1
         ORDER BY sort_order, name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;
    let methods = method_rows
        .iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            Ok(AdminMethodView {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                kind_label: payment_kind_label(&kind),
                currency: row.try_get("currency")?,
                sort_order: row.try_get("sort_order")?,
                is_active: row.try_get("is_active")?,
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let category_form = load_category_form(&state, &session, &query.edit_category).await?;
    let item_form = load_item_form(&state, &session, &query.edit_item).await?;
    let table_form = load_table_form(&state, &session, &query.edit_table).await?;
    let user_form = load_user_form(&state, &session, &query.edit_user).await?;
    let method_form = load_method_form(&state, &session, &query.edit_method).await?;

    let html = AdminTemplate {
        common: page_context(&session),
        active: "admin",
        business_name: business.try_get("name")?,
        legal_name: business
            .try_get::<Option<String>, _>("legal_name")?
            .unwrap_or_default(),
        tax_id: business
            .try_get::<Option<String>, _>("tax_id")?
            .unwrap_or_default(),
        base_currency,
        exchange_rate: if exchange_rate_micros > 0 {
            billing::format_rate_micros(exchange_rate_micros)
        } else {
            String::new()
        },
        tax_name,
        tax_rate: format_tax_percent(tax_rate_basis_points),
        tax_enabled,
        categories,
        items,
        tables,
        users,
        methods,
        category_form,
        item_form,
        table_form,
        user_form,
        method_form,
    }
    .render()?;

    Ok(Html(html).into_response())
}

async fn require_admin(state: &AppState, jar: &CookieJar) -> Result<Option<SessionUser>, AppError> {
    let Some(session) = auth::web_session(&state.pool, jar).await? else {
        return Ok(None);
    };
    if session.role != "admin" {
        return Err(AppError::Forbidden);
    }

    Ok(Some(session))
}

async fn load_category_form(
    state: &AppState,
    session: &SessionUser,
    id: &str,
) -> Result<AdminCategoryEdit, AppError> {
    if id.trim().is_empty() {
        return Ok(AdminCategoryEdit {
            id: String::new(),
            name: String::new(),
            sort_order: 0,
            is_active: true,
        });
    }

    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, name, sort_order, is_active
         FROM menu_category WHERE id = ?1 AND business_id = ?2",
    )
    .bind(id.trim())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(AdminCategoryEdit {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        sort_order: row.try_get("sort_order")?,
        is_active: row.try_get("is_active")?,
    })
}

async fn load_item_form(
    state: &AppState,
    session: &SessionUser,
    id: &str,
) -> Result<AdminItemEdit, AppError> {
    if id.trim().is_empty() {
        return Ok(AdminItemEdit {
            id: String::new(),
            name: String::new(),
            description: String::new(),
            category_id: String::new(),
            price: String::new(),
            sort_order: 0,
            is_active: true,
            is_available: true,
        });
    }

    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, name, COALESCE(description, '') AS description,
                COALESCE(category_id, '') AS category_id, price_minor,
                sort_order, is_active, is_available
         FROM menu_item WHERE id = ?1 AND business_id = ?2",
    )
    .bind(id.trim())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(AdminItemEdit {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        category_id: row.try_get("category_id")?,
        price: format_amount_input(row.try_get("price_minor")?),
        sort_order: row.try_get("sort_order")?,
        is_active: row.try_get("is_active")?,
        is_available: row.try_get("is_available")?,
    })
}

async fn load_table_form(
    state: &AppState,
    session: &SessionUser,
    id: &str,
) -> Result<AdminTableEdit, AppError> {
    if id.trim().is_empty() {
        return Ok(AdminTableEdit {
            id: String::new(),
            name: String::new(),
            zone: String::new(),
            is_active: true,
        });
    }

    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, name, COALESCE(zone, '') AS zone, is_active
         FROM dining_table WHERE id = ?1 AND business_id = ?2",
    )
    .bind(id.trim())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(AdminTableEdit {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        zone: row.try_get("zone")?,
        is_active: row.try_get("is_active")?,
    })
}

async fn load_user_form(
    state: &AppState,
    session: &SessionUser,
    id: &str,
) -> Result<AdminUserEdit, AppError> {
    if id.trim().is_empty() {
        return Ok(AdminUserEdit {
            id: String::new(),
            name: String::new(),
            role: "waiter".to_owned(),
            is_active: true,
        });
    }

    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, display_name, role, is_active
         FROM app_user WHERE id = ?1 AND business_id = ?2",
    )
    .bind(id.trim())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(AdminUserEdit {
        id: row.try_get("id")?,
        name: row.try_get("display_name")?,
        role: row.try_get("role")?,
        is_active: row.try_get("is_active")?,
    })
}

async fn load_method_form(
    state: &AppState,
    session: &SessionUser,
    id: &str,
) -> Result<AdminMethodEdit, AppError> {
    if id.trim().is_empty() {
        return Ok(AdminMethodEdit {
            id: String::new(),
            name: String::new(),
            kind: "cash".to_owned(),
            currency: String::new(),
            sort_order: 0,
            is_active: true,
        });
    }

    use sqlx::Row;
    let row = sqlx::query(
        "SELECT id, name, kind, COALESCE(currency, '') AS currency,
                sort_order, is_active
         FROM payment_method WHERE id = ?1 AND business_id = ?2",
    )
    .bind(id.trim())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    Ok(AdminMethodEdit {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        kind: row.try_get("kind")?,
        currency: row.try_get("currency")?,
        sort_order: row.try_get("sort_order")?,
        is_active: row.try_get("is_active")?,
    })
}

fn format_amount_input(minor: i64) -> String {
    format!("{}.{:02}", minor / 100, minor % 100)
}

fn format_tax_percent(basis_points: i64) -> String {
    if basis_points % 100 == 0 {
        return (basis_points / 100).to_string();
    }

    format!("{}.{:02}", basis_points / 100, basis_points % 100)
        .trim_end_matches('0')
        .to_owned()
}

fn parse_tax_percent(input: &str) -> Result<i64, AppError> {
    let value = input.trim().replace(',', ".");
    if value.is_empty() {
        return Err(AppError::Validation("enter the IVA rate".to_owned()));
    }
    let (whole, fraction) = value.split_once('.').unwrap_or((value.as_str(), ""));
    if fraction.len() > 2
        || !fraction.chars().all(|character| character.is_ascii_digit())
        || !whole.chars().all(|character| character.is_ascii_digit())
    {
        return Err(AppError::Validation(
            "the IVA rate can have at most two decimals".to_owned(),
        ));
    }

    let whole: i64 = whole
        .parse()
        .map_err(|_| AppError::Validation("the IVA rate is too large".to_owned()))?;
    let fraction: i64 = match fraction.len() {
        0 => 0,
        1 => {
            fraction
                .parse::<i64>()
                .map_err(|_| AppError::Validation("invalid IVA rate".to_owned()))?
                * 10
        }
        _ => fraction
            .parse()
            .map_err(|_| AppError::Validation("invalid IVA rate".to_owned()))?,
    };

    let basis_points = whole
        .checked_mul(100)
        .and_then(|value| value.checked_add(fraction))
        .ok_or_else(|| AppError::Validation("the IVA rate is too large".to_owned()))?;
    if !(0..=10_000).contains(&basis_points) {
        return Err(AppError::Validation(
            "the IVA rate must be between 0 and 100".to_owned(),
        ));
    }

    Ok(basis_points)
}

fn admin_bool(value: &str) -> bool {
    matches!(value, "true" | "on" | "1")
}

fn parse_sort_order(value: &str) -> Result<i64, AppError> {
    if value.trim().is_empty() {
        return Ok(0);
    }
    let order: i64 = value
        .trim()
        .parse()
        .map_err(|_| AppError::Validation("invalid sort order".to_owned()))?;
    if order < 0 {
        return Err(AppError::Validation(
            "the sort order cannot be negative".to_owned(),
        ));
    }
    Ok(order)
}

fn admin_entity_id(id: &str) -> Result<Uuid, AppError> {
    if id.trim().is_empty() {
        return Ok(Uuid::now_v7());
    }
    id.trim()
        .parse::<Uuid>()
        .map_err(|_| AppError::Validation("invalid record id".to_owned()))
}

fn admin_required_name(name: &str) -> Result<String, AppError> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Validation("the name is required".to_owned()));
    }
    Ok(name.to_owned())
}

/// Saves the business name, exchange rate, and IVA settings.
pub async fn admin_business(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<BusinessAdminForm>,
) -> Result<Response, AppError> {
    let Some(session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    use sqlx::Row;
    let business = sqlx::query("SELECT base_currency, locale FROM business WHERE id = ?1")
        .bind(&session.business_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or(AppError::NotFound)?;
    let base_currency: String = business.try_get("base_currency")?;
    let locale: String = business.try_get("locale")?;
    let name = admin_required_name(&form.name)?;
    let exchange_rate_micros = if form.exchange_rate.trim().is_empty() {
        0
    } else {
        billing::parse_rate_micros(&form.exchange_rate)?
    };
    let tax_rate = parse_tax_percent(&form.tax_rate)?;
    let tax_name = if form.tax_name.trim().is_empty() {
        "IVA".to_owned()
    } else {
        form.tax_name.trim().to_owned()
    };
    let tax_enabled = admin_bool(&form.tax_enabled);
    let tax_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM tax_rate WHERE business_id = ?1 LIMIT 1")
            .bind(&session.business_id)
            .fetch_optional(&state.pool)
            .await?;
    let tax_id = tax_id
        .as_deref()
        .map(str::parse::<Uuid>)
        .transpose()
        .map_err(|_| AppError::CorruptData("invalid tax rate id".to_owned()))?
        .unwrap_or_else(Uuid::now_v7);

    let device_id = web_device(&state, &session).await?;
    let branch_id = cash_branch(&state, &session, &device_id).await?;
    let business_id = session
        .business_id
        .parse::<Uuid>()
        .map_err(|_| AppError::CorruptData("invalid business id".to_owned()))?;
    let now = now_ms();
    let operations = vec![
        WebOperation {
            operation: Operation::new(
                device_id,
                Some(branch_id),
                OperationKind::BusinessConfigured,
                business_id,
                json!({
                    "name": name,
                    "legal_name": clean_optional(&form.legal_name),
                    "tax_id": clean_optional(&form.tax_id),
                    "base_currency": base_currency,
                    "locale": locale,
                }),
                now,
            ),
            action: "business_configured".to_owned(),
            details: json!({ "name": form.name.trim() }),
        },
        WebOperation {
            operation: Operation::new(
                device_id,
                Some(branch_id),
                OperationKind::SettingChanged,
                Uuid::now_v7(),
                json!({
                    "key": "exchange_rate_micros",
                    "value": exchange_rate_micros.to_string(),
                }),
                now,
            ),
            action: "setting_changed".to_owned(),
            details: json!({ "key": "exchange_rate_micros" }),
        },
        WebOperation {
            operation: Operation::new(
                device_id,
                Some(branch_id),
                OperationKind::TaxRateChanged,
                tax_id,
                json!({
                    "name": tax_name,
                    "rate_basis_points": tax_rate,
                    "enabled": tax_enabled,
                }),
                now,
            ),
            action: "tax_rate_changed".to_owned(),
            details: json!({ "rate_basis_points": tax_rate, "enabled": tax_enabled }),
        },
    ];

    sync::append_web_operations(
        &state.pool,
        operations,
        &session.user_id,
        &session.business_id,
        Some(&branch_id.to_string()),
    )
    .await?;

    Ok(Redirect::to("/app/admin#business").into_response())
}

/// Creates or updates a menu category.
pub async fn admin_category(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<CategoryAdminForm>,
) -> Result<Response, AppError> {
    let Some(session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    let category_id = admin_entity_id(&form.id)?;
    let name = admin_required_name(&form.name)?;
    let sort_order = parse_sort_order(&form.sort_order)?;
    let is_active = admin_bool(&form.is_active);

    append_admin_operation(
        &state,
        &session,
        OperationKind::MenuCategoryUpserted,
        category_id,
        json!({
            "name": name,
            "sort_order": sort_order,
            "is_active": is_active,
        }),
        "menu_category_upserted",
    )
    .await?;

    Ok(Redirect::to("/app/admin#menu").into_response())
}

/// Creates or updates a menu item.
pub async fn admin_item(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<ItemAdminForm>,
) -> Result<Response, AppError> {
    let Some(session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    let item_id = admin_entity_id(&form.id)?;
    let name = admin_required_name(&form.name)?;
    let price_minor = billing::parse_amount_minor(&form.price)?;
    let sort_order = parse_sort_order(&form.sort_order)?;
    let is_active = admin_bool(&form.is_active);
    let is_available = admin_bool(&form.is_available);
    let base_currency = base_currency(&state.pool, &session.business_id).await?;
    let category_id = clean_optional(&form.category_id).map(str::to_owned);
    if let Some(category_id) = &category_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM menu_category WHERE id = ?1 AND business_id = ?2
             )",
        )
        .bind(category_id)
        .bind(&session.business_id)
        .fetch_one(&state.pool)
        .await?;
        if !exists {
            return Err(AppError::Validation(
                "the menu category does not exist".to_owned(),
            ));
        }
    }

    append_admin_operation(
        &state,
        &session,
        OperationKind::MenuItemUpserted,
        item_id,
        json!({
            "category_id": category_id,
            "name": name,
            "description": clean_optional(&form.description),
            "price_minor": price_minor,
            "currency": base_currency,
            "is_active": is_active,
            "is_available": is_available,
            "sort_order": sort_order,
        }),
        "menu_item_upserted",
    )
    .await?;

    Ok(Redirect::to("/app/admin#menu").into_response())
}

/// Creates or updates a dining table.
pub async fn admin_table(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<TableAdminForm>,
) -> Result<Response, AppError> {
    let Some(session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    let table_id = admin_entity_id(&form.id)?;
    let name = admin_required_name(&form.name)?;
    let is_active = admin_bool(&form.is_active);

    append_admin_operation(
        &state,
        &session,
        OperationKind::DiningTableUpserted,
        table_id,
        json!({
            "name": name,
            "zone": clean_optional(&form.zone),
            "is_active": is_active,
        }),
        "dining_table_upserted",
    )
    .await?;

    Ok(Redirect::to("/app/admin#tables").into_response())
}

/// Creates or updates a user and optionally resets the PIN.
pub async fn admin_user(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<UserAdminForm>,
) -> Result<Response, AppError> {
    let Some(session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    let user_id = admin_entity_id(&form.id)?;
    let display_name = admin_required_name(&form.display_name)?;
    if !matches!(
        form.role.as_str(),
        "admin" | "cashier" | "waiter" | "kitchen"
    ) {
        return Err(AppError::Validation("invalid user role".to_owned()));
    }
    let is_active = admin_bool(&form.is_active);
    let pin = form.pin.trim();
    if !form.id.trim().is_empty() && pin.is_empty() {
        // Keep the current PIN.
    } else {
        auth::validate_pin(pin)?;
    }

    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM app_user
            WHERE business_id = ?1 AND display_name = ?2 AND id <> ?3
         )",
    )
    .bind(&session.business_id)
    .bind(&display_name)
    .bind(form.id.trim())
    .fetch_one(&state.pool)
    .await?;
    if duplicate {
        return Err(AppError::Validation(
            "a user with that name already exists".to_owned(),
        ));
    }
    let pin_hash = if pin.is_empty() {
        None
    } else {
        Some(auth::hash_pin(pin)?)
    };

    if form.id.trim() == session.user_id {
        if !is_active {
            return Err(AppError::Validation(
                "you cannot deactivate your own user".to_owned(),
            ));
        }
        if form.role != "admin" {
            return Err(AppError::Validation(
                "you cannot remove your own admin role".to_owned(),
            ));
        }
    }

    if !form.id.trim().is_empty() {
        let existing: Option<(String, bool)> = sqlx::query_as(
            "SELECT role, is_active FROM app_user
             WHERE id = ?1 AND business_id = ?2",
        )
        .bind(form.id.trim())
        .bind(&session.business_id)
        .fetch_optional(&state.pool)
        .await?;
        if let Some((role, was_active)) = existing
            && role == "admin"
            && was_active
            && (form.role != "admin" || !is_active)
        {
            let active_admins: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM app_user
                 WHERE business_id = ?1 AND role = 'admin' AND is_active = 1",
            )
            .bind(&session.business_id)
            .fetch_one(&state.pool)
            .await?;
            if active_admins <= 1 {
                return Err(AppError::Validation(
                    "the business needs at least one active admin".to_owned(),
                ));
            }
        }
    }

    let payload = match pin_hash {
        Some(pin_hash) => json!({
            "display_name": display_name,
            "role": form.role,
            "is_active": is_active,
            "pin_hash": pin_hash,
        }),
        None => json!({
            "display_name": display_name,
            "role": form.role,
            "is_active": is_active,
        }),
    };

    append_admin_operation(
        &state,
        &session,
        OperationKind::UserUpserted,
        user_id,
        payload,
        "user_upserted",
    )
    .await?;

    Ok(Redirect::to("/app/admin#users").into_response())
}

/// Creates or updates a payment method.
pub async fn admin_payment_method(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<PaymentMethodAdminForm>,
) -> Result<Response, AppError> {
    let Some(session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };
    if !auth::csrf_matches(&session, &form.csrf_token) {
        return Err(AppError::Forbidden);
    }

    let method_id = admin_entity_id(&form.id)?;
    let name = admin_required_name(&form.name)?;
    if !matches!(
        form.kind.as_str(),
        "cash" | "card" | "transfer" | "mobile_payment" | "other"
    ) {
        return Err(AppError::Validation(
            "invalid payment method kind".to_owned(),
        ));
    }
    let currency = form.currency.trim().to_uppercase();
    if !currency.is_empty() && currency.len() != 3 {
        return Err(AppError::Validation(
            "the payment currency must have three letters".to_owned(),
        ));
    }
    let currency = (!currency.is_empty()).then_some(currency);
    let sort_order = parse_sort_order(&form.sort_order)?;
    let is_active = admin_bool(&form.is_active);

    let duplicate: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM payment_method
            WHERE business_id = ?1 AND name = ?2 AND id <> ?3
         )",
    )
    .bind(&session.business_id)
    .bind(&name)
    .bind(form.id.trim())
    .fetch_one(&state.pool)
    .await?;
    if duplicate {
        return Err(AppError::Validation(
            "a payment method with that name already exists".to_owned(),
        ));
    }

    append_admin_operation(
        &state,
        &session,
        OperationKind::PaymentMethodUpserted,
        method_id,
        json!({
            "name": name,
            "kind": form.kind,
            "currency": currency,
            "sort_order": sort_order,
            "is_active": is_active,
        }),
        "payment_method_upserted",
    )
    .await?;

    Ok(Redirect::to("/app/admin#payments").into_response())
}

async fn append_admin_operation(
    state: &AppState,
    session: &SessionUser,
    kind: OperationKind,
    entity_id: Uuid,
    payload: Value,
    action: &str,
) -> Result<(), AppError> {
    let device_id = web_device(state, session).await?;
    let branch_id = cash_branch(state, session, &device_id).await?;
    let operation = Operation::new(
        device_id,
        Some(branch_id),
        kind,
        entity_id,
        payload,
        now_ms(),
    );

    sync::append_web_operation(
        &state.pool,
        &operation,
        &session.user_id,
        &session.business_id,
        Some(&branch_id.to_string()),
        action,
        json!({ "entity_id": entity_id.to_string() }),
    )
    .await
}

/// Downloads a consistent SQLite copy of the live database.
pub async fn admin_backup(
    State(state): State<AppState>,
    jar: CookieJar,
) -> Result<Response, AppError> {
    let Some(_session) = require_admin(&state, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let path = std::env::temp_dir().join(format!("nexo-backup-{}.db", Uuid::now_v7()));
    let path_text = path.to_string_lossy().to_string();
    sqlx::query("VACUUM INTO ?1")
        .bind(&path_text)
        .execute(&state.pool)
        .await?;
    let bytes = tokio::fs::read(&path).await?;
    let _ = tokio::fs::remove_file(&path).await;

    let filename = format!("nexo-backup-{}.db", today_local_date());
    let headers = [
        (
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/octet-stream"),
        ),
        (
            header::CONTENT_DISPOSITION,
            HeaderValue::try_from(format!("attachment; filename=\"{filename}\""))
                .map_err(|_| AppError::Internal("invalid backup filename".to_owned()))?,
        ),
    ];

    Ok((headers, bytes).into_response())
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
    let can_cancel = matches!(session.role.as_str(), "admin" | "cashier") && !is_terminal;
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

    let (active, back_href, back_label) = if detail.order.order_type == "takeout" {
        ("takeout", "/app/takeout", "Volver a para llevar")
    } else {
        ("tables", "/app/tables", "Volver a mesas")
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
        can_cancel,
        active,
        back_href,
        back_label,
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

/// Returns the browser device, creating one for sessions created before
/// per-browser devices existed.
async fn web_device(state: &AppState, session: &SessionUser) -> Result<DeviceId, AppError> {
    if let Some(device_id) = &session.device_id {
        return device_id
            .parse::<DeviceId>()
            .map_err(|_| AppError::CorruptData("invalid web device id".to_owned()));
    }

    auth::create_web_device(&state.pool, &session.business_id, &session.session_id).await
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

const TABLE_ROLES: &[&str] = &["admin", "cashier", "waiter"];
const KITCHEN_ROLES: &[&str] = &["admin", "kitchen"];

fn require_roles(session: &SessionUser, roles: &[&str]) -> Result<(), AppError> {
    if roles.contains(&session.role.as_str()) {
        Ok(())
    } else {
        Err(AppError::Forbidden)
    }
}

fn forbidden_page() -> Result<Response, AppError> {
    let html = ForbiddenTemplate.render()?;

    Ok((StatusCode::FORBIDDEN, Html(html)).into_response())
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

    let device_id = web_device(&state, &session).await?;
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

fn parse_form(body: &[u8]) -> Vec<(String, String)> {
    form_urlencoded::parse(body)
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect()
}

fn first_value<'a>(form: &'a [(String, String)], key: &str) -> Option<&'a str> {
    form.iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

fn all_values<'a>(form: &'a [(String, String)], key: &str) -> Vec<&'a str> {
    form.iter()
        .filter(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .collect()
}

fn clean_optional(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then_some(trimmed)
}

fn format_rate_label(rate_basis_points: i64) -> String {
    let whole = rate_basis_points / 100;
    let fraction = rate_basis_points % 100;
    if fraction == 0 {
        return format!("{whole}%");
    }

    format!("{whole}.{fraction:02}%")
        .trim_end_matches('0')
        .to_owned()
        .replace(".%", "%")
}

fn payment_kind_label(kind: &str) -> &'static str {
    match kind {
        "cash" => "Efectivo",
        "card" => "Tarjeta",
        "transfer" => "Transferencia",
        "mobile_payment" => "Pago móvil",
        _ => "Otro",
    }
}

fn movement_kind_label(kind: &str) -> &'static str {
    match kind {
        "in" => "Entrada",
        "out" => "Salida",
        "sale" => "Venta",
        "refund" => "Reembolso",
        "expense" => "Gasto",
        "withdrawal" => "Retiro",
        "deposit" => "Depósito",
        _ => "Movimiento",
    }
}

async fn tax_config(
    pool: &sqlx::SqlitePool,
    business_id: &str,
) -> Result<Option<(String, i64, bool)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT name, rate_basis_points, is_enabled
         FROM tax_rate
         WHERE business_id = ?1
         LIMIT 1",
    )
    .bind(business_id)
    .fetch_optional(pool)
    .await?)
}

async fn exchange_rate(pool: &sqlx::SqlitePool, business_id: &str) -> Result<i64, AppError> {
    let value: Option<String> = sqlx::query_scalar(
        "SELECT value FROM business_setting
         WHERE business_id = ?1 AND key = 'exchange_rate_micros'",
    )
    .bind(business_id)
    .fetch_optional(pool)
    .await?;

    Ok(value
        .and_then(|value| value.parse::<i64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(0))
}

async fn sale_for_order(
    pool: &sqlx::SqlitePool,
    order_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM sale WHERE order_id = ?1 LIMIT 1")
            .bind(order_id)
            .fetch_optional(pool)
            .await?,
    )
}

async fn payment_method(
    pool: &sqlx::SqlitePool,
    business_id: &str,
    method_id: &str,
) -> Result<Option<(String, String, Option<String>, bool)>, AppError> {
    Ok(sqlx::query_as(
        "SELECT name, kind, currency, is_active
         FROM payment_method
         WHERE id = ?1 AND business_id = ?2",
    )
    .bind(method_id)
    .bind(business_id)
    .fetch_optional(pool)
    .await?)
}

async fn load_payment_methods(
    state: &AppState,
    session: &SessionUser,
) -> Result<Vec<PaymentMethodView>, AppError> {
    let rows = sqlx::query(
        "SELECT id, name, kind, currency
         FROM payment_method
         WHERE business_id = ?1 AND is_active = 1
         ORDER BY sort_order, name",
    )
    .bind(&session.business_id)
    .fetch_all(&state.pool)
    .await?;

    use sqlx::Row;
    rows.iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            Ok(PaymentMethodView {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                kind_label: payment_kind_label(&kind),
                currency: row
                    .try_get::<Option<String>, _>("currency")?
                    .unwrap_or_default(),
            })
        })
        .collect()
}

async fn next_sale_number(pool: &sqlx::SqlitePool, branch_id: &str) -> Result<String, AppError> {
    let last: i64 = sqlx::query_scalar(
        "SELECT COALESCE(MAX(CAST(SUBSTR(number, 3) AS INTEGER)), 0)
         FROM sale
         WHERE branch_id = ?1 AND number LIKE 'V-%'",
    )
    .bind(branch_id)
    .fetch_one(pool)
    .await?;

    Ok(format!("V-{:04}", last + 1))
}

async fn cash_branch(
    state: &AppState,
    session: &SessionUser,
    device_id: &DeviceId,
) -> Result<BranchId, AppError> {
    let branch: Option<String> = sqlx::query_scalar(
        "SELECT COALESCE(
            d.branch_id,
            (SELECT id FROM branch
             WHERE business_id = d.business_id
             ORDER BY is_default DESC, created_at_ms
             LIMIT 1)
         )
         FROM device d
         WHERE d.id = ?1 AND d.business_id = ?2",
    )
    .bind(device_id.to_string())
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?;

    let Some(branch) = branch else {
        return Err(AppError::NotFound);
    };

    branch
        .parse::<BranchId>()
        .map_err(|_| AppError::CorruptData("invalid branch id".to_owned()))
}

struct OpenCashSessionRow {
    id: String,
    currency: String,
    secondary_currency: String,
}

async fn open_cash_session_row(
    pool: &sqlx::SqlitePool,
    branch_id: &BranchId,
    device_id: &DeviceId,
) -> Result<Option<OpenCashSessionRow>, AppError> {
    let row = sqlx::query(
        "SELECT id, currency, secondary_currency
         FROM cash_session
         WHERE branch_id = ?1 AND device_id = ?2 AND status = 'open'
         LIMIT 1",
    )
    .bind(branch_id.to_string())
    .bind(device_id.to_string())
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    use sqlx::Row;
    Ok(Some(OpenCashSessionRow {
        id: row.try_get("id")?,
        currency: row.try_get("currency")?,
        secondary_currency: row.try_get("secondary_currency")?,
    }))
}

async fn cash_expected(
    pool: &sqlx::SqlitePool,
    cash_session_id: &str,
    base: &str,
    secondary: &str,
) -> Result<(i64, i64), AppError> {
    let (opening_base, opening_secondary): (i64, i64) = sqlx::query_as(
        "SELECT opening_amount_minor, secondary_opening_amount_minor
         FROM cash_session
         WHERE id = ?1",
    )
    .bind(cash_session_id)
    .fetch_one(pool)
    .await?;

    let rows = sqlx::query(
        "SELECT kind, currency, SUM(amount_minor) AS total
         FROM cash_movement
         WHERE cash_session_id = ?1
         GROUP BY kind, currency",
    )
    .bind(cash_session_id)
    .fetch_all(pool)
    .await?;

    use sqlx::Row;
    let mut expected_base = opening_base;
    let mut expected_secondary = opening_secondary;

    for row in rows {
        let kind: String = row.try_get("kind")?;
        let currency: String = row.try_get("currency")?;
        let total: i64 = row.try_get("total")?;
        let signed = match kind.as_str() {
            "in" | "deposit" | "sale" => total,
            "out" | "expense" | "withdrawal" | "refund" => -total,
            _ => 0,
        };

        if currency == base {
            expected_base += signed;
        } else if currency == secondary {
            expected_secondary += signed;
        }
    }

    Ok((expected_base, expected_secondary))
}

async fn load_cash_session(
    state: &AppState,
    device_id: &DeviceId,
    branch_id: &BranchId,
) -> Result<Option<CashSessionView>, AppError> {
    let Some(open) = open_cash_session_row(&state.pool, branch_id, device_id).await? else {
        return Ok(None);
    };

    use sqlx::Row;
    let row = sqlx::query(
        "SELECT s.opened_at_ms, COALESCE(u.display_name, '—') AS opened_by
         FROM cash_session s
         LEFT JOIN app_user u ON u.id = s.opened_by
         WHERE s.id = ?1",
    )
    .bind(&open.id)
    .fetch_one(&state.pool)
    .await?;

    let (expected_base, expected_secondary) = cash_expected(
        &state.pool,
        &open.id,
        &open.currency,
        &open.secondary_currency,
    )
    .await?;

    let movement_rows = sqlx::query(
        "SELECT kind, amount_minor, currency, reason, created_at_ms
         FROM cash_movement
         WHERE cash_session_id = ?1
         ORDER BY created_at_ms, id",
    )
    .bind(&open.id)
    .fetch_all(&state.pool)
    .await?;

    let movements = movement_rows
        .iter()
        .map(|row| {
            let kind: String = row.try_get("kind")?;
            let amount_minor: i64 = row.try_get("amount_minor")?;
            let currency: String = row.try_get("currency")?;
            Ok(CashMovementView {
                kind_label: movement_kind_label(&kind),
                amount: format_price(amount_minor, &currency),
                reason: row
                    .try_get::<Option<String>, _>("reason")?
                    .unwrap_or_default(),
                created_at: format_local_time(row.try_get("created_at_ms")?),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let opening_base: i64 =
        sqlx::query_scalar("SELECT opening_amount_minor FROM cash_session WHERE id = ?1")
            .bind(&open.id)
            .fetch_one(&state.pool)
            .await?;
    let opening_secondary: i64 =
        sqlx::query_scalar("SELECT secondary_opening_amount_minor FROM cash_session WHERE id = ?1")
            .bind(&open.id)
            .fetch_one(&state.pool)
            .await?;

    Ok(Some(CashSessionView {
        opened_at: format_local_time(row.try_get("opened_at_ms")?),
        opened_by: row.try_get("opened_by")?,
        status_label: "Abierta",
        base_currency: open.currency.clone(),
        secondary_currency: open.secondary_currency.clone(),
        opening_base: format_price(opening_base, &open.currency),
        opening_secondary: format_price(opening_secondary, &open.secondary_currency),
        expected_base: format_price(expected_base, &open.currency),
        expected_secondary: format_price(expected_secondary, &open.secondary_currency),
        movements,
    }))
}

async fn load_receipt(
    state: &AppState,
    session: &SessionUser,
    sale_id: &str,
) -> Result<ReceiptView, AppError> {
    use sqlx::Row;
    let row = sqlx::query(
        "SELECT
            s.number, s.created_at_ms, s.status, s.subtotal_minor, s.tax_minor,
            s.total_minor, s.currency, s.tax_name, s.tax_rate_basis_points,
            COALESCE(u.display_name, '—') AS cashier,
            o.order_type, o.display_code, o.customer_name,
            t.name AS table_name
         FROM sale s
         JOIN restaurant_order o ON o.id = s.order_id
         LEFT JOIN dining_table t ON t.id = o.table_id
         LEFT JOIN app_user u ON u.id = s.created_by
         WHERE s.id = ?1 AND s.business_id = ?2",
    )
    .bind(sale_id)
    .bind(&session.business_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or(AppError::NotFound)?;

    let currency: String = row.try_get("currency")?;
    let subtotal_minor: i64 = row.try_get("subtotal_minor")?;
    let tax_minor: i64 = row.try_get("tax_minor")?;
    let total_minor: i64 = row.try_get("total_minor")?;
    let tax_name: Option<String> = row.try_get("tax_name")?;
    let tax_rate: Option<i64> = row.try_get("tax_rate_basis_points")?;
    let order_type: String = row.try_get("order_type")?;

    let line_rows = sqlx::query(
        "SELECT description, quantity, line_total_minor
         FROM sale_line
         WHERE sale_id = ?1
         ORDER BY created_at_ms, id",
    )
    .bind(sale_id)
    .fetch_all(&state.pool)
    .await?;
    let lines = line_rows
        .iter()
        .map(|row| {
            Ok(ReceiptLineView {
                description: row.try_get("description")?,
                quantity: row.try_get("quantity")?,
                total: format_price(row.try_get("line_total_minor")?, &currency),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let payment_rows = sqlx::query(
        "SELECT
            COALESCE(m.name, 'Pago') AS method,
            p.amount_minor, p.currency, p.tendered_minor, p.change_minor,
            p.reference, p.exchange_rate_micros
         FROM payment p
         LEFT JOIN payment_method m ON m.id = p.method_id
         WHERE p.sale_id = ?1
         ORDER BY p.created_at_ms, p.id",
    )
    .bind(sale_id)
    .fetch_all(&state.pool)
    .await?;
    let payments = payment_rows
        .iter()
        .map(|row| {
            let currency: String = row.try_get("currency")?;
            let tendered: Option<i64> = row.try_get("tendered_minor")?;
            let change: Option<i64> = row.try_get("change_minor")?;
            let rate: Option<i64> = row.try_get("exchange_rate_micros")?;
            Ok(ReceiptPaymentView {
                method: row.try_get("method")?,
                amount: format_price(row.try_get("amount_minor")?, &currency),
                tendered: tendered
                    .map(|value| format_price(value, &currency))
                    .unwrap_or_default(),
                change: change
                    .map(|value| format_price(value, &currency))
                    .unwrap_or_default(),
                reference: row
                    .try_get::<Option<String>, _>("reference")?
                    .unwrap_or_default(),
                exchange_rate: rate.map(billing::format_rate_micros).unwrap_or_default(),
            })
        })
        .collect::<Result<Vec<_>, AppError>>()?;

    let tax_label = match (tax_name, tax_rate) {
        (Some(name), Some(rate)) if tax_minor > 0 => {
            format!("{name} incluido ({})", format_rate_label(rate))
        }
        (Some(name), _) if tax_minor > 0 => format!("{name} incluido"),
        _ => "IVA".to_owned(),
    };

    Ok(ReceiptView {
        number: row.try_get("number")?,
        business_name: session.business_name.clone(),
        order_label: order_label(
            &order_type,
            row.try_get::<Option<String>, _>("table_name")?.as_deref(),
            row.try_get::<Option<String>, _>("customer_name")?
                .as_deref(),
            &row.try_get::<String, _>("display_code")?,
        ),
        display_code: row.try_get("display_code")?,
        created_at: format_local_time(row.try_get("created_at_ms")?),
        cashier: row.try_get("cashier")?,
        status_label: match row.try_get::<String, _>("status")?.as_str() {
            "cancelled" => "Anulada",
            _ => "Pagada",
        },
        subtotal: format_price(subtotal_minor, &currency),
        tax: format_price(tax_minor, &currency),
        tax_label,
        total: format_price(total_minor, &currency),
        has_tax: tax_minor > 0,
        lines,
        payments,
    })
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

/// Web app manifest used by the home-screen shortcut.
pub async fn manifest() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/manifest+json")],
        MANIFEST,
    )
}

/// Scalable application icon.
pub async fn icon_svg() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/svg+xml")], ICON_SVG)
}

/// 192 px application icon.
pub async fn icon_192() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], ICON_192)
}

/// 512 px application icon.
pub async fn icon_512() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], ICON_512)
}

/// Apple touch icon for the iOS home screen.
pub async fn apple_touch_icon() -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "image/png")], APPLE_TOUCH_ICON)
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
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find_map(|value| {
                let cookie = value.split(';').next()?;
                cookie
                    .starts_with("nexo_session=")
                    .then(|| cookie.to_owned())
            })
            .expect("login sets a session cookie")
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

    async fn open_cash_session(state: &AppState, cookie: &str) -> String {
        let csrf = csrf_for(state, "Ana").await;
        let response = post_form(
            state,
            "/app/cash/open",
            cookie,
            &format!(
                "csrf_token={csrf}&opening_base=100&opening_secondary=3600&notes=Turno+de+prueba"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        sqlx::query_scalar("SELECT id FROM cash_session WHERE status = 'open' LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    async fn payment_method_id(state: &AppState) -> String {
        sqlx::query_scalar("SELECT id FROM payment_method ORDER BY sort_order, name LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap()
    }

    fn location(response: &Response) -> String {
        response
            .headers()
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned()
    }

    #[tokio::test]
    async fn cashier_bills_an_order_with_cash_and_change() {
        let (state, cookie, item_id) = linked_state().await;
        let (order_id, _) = open_sent_order(&state, &cookie, &item_id).await;
        let cash_session_id = open_cash_session(&state, &cookie).await;
        let method_id = payment_method_id(&state).await;
        let csrf = csrf_for(&state, "Ana").await;

        let page = get_page(&state, &format!("/app/orders/{order_id}/bill"), &cookie).await;
        assert_eq!(page.status(), StatusCode::OK);
        let body = body_text(page).await;
        assert!(body.contains("Cobrar y cerrar pedido"));
        assert!(body.contains("5.00 USD"));
        assert!(!body.contains("requieren una caja abierta"));

        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/bill"),
            &cookie,
            &format!(
                "csrf_token={csrf}&exchange_rate=36&method_id={method_id}\
                 &currency=USD&amount=6&reference="
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        let sale_location = location(&response);
        assert!(sale_location.starts_with("/app/sales/"));

        let sale: (String, i64, i64, i64, String) = sqlx::query_as(
            "SELECT number, subtotal_minor, tax_minor, total_minor, status
             FROM sale WHERE order_id = ?1",
        )
        .bind(&order_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(sale.0, "V-0001");
        assert_eq!(sale.1, 500);
        assert_eq!(sale.2, 0);
        assert_eq!(sale.3, 500);
        assert_eq!(sale.4, "completed");

        let payment: (i64, i64, i64) = sqlx::query_as(
            "SELECT amount_minor, tendered_minor, change_minor
             FROM payment ORDER BY created_at_ms LIMIT 1",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(payment, (500, 600, 100));

        let movement: (String, i64, String) = sqlx::query_as(
            "SELECT kind, amount_minor, reference_id
             FROM cash_movement WHERE cash_session_id = ?1",
        )
        .bind(&cash_session_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(movement.0, "sale");
        assert_eq!(movement.1, 500);
        assert_eq!(movement.2, sale_location.rsplit('/').next().unwrap());

        let order_status: String =
            sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
                .bind(&order_id)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(order_status, "billed");

        let receipt = get_page(&state, &sale_location, &cookie).await;
        assert_eq!(receipt.status(), StatusCode::OK);
        let body = body_text(receipt).await;
        assert!(body.contains("V-0001"));
        assert!(body.contains("Recibido 6.00 USD"));
        assert!(body.contains("Cambio 1.00 USD"));
        assert!(body.contains("Gracias por su compra"));

        let cashier = get_page(&state, "/app/cashier", &cookie).await;
        assert_eq!(cashier.status(), StatusCode::OK);
        let body = body_text(cashier).await;
        assert!(!body.contains("Arepa"));
        assert!(body.contains("No hay pedidos abiertos"));
    }

    #[tokio::test]
    async fn cashier_splits_a_payment_between_dollars_and_bolivars() {
        let (state, cookie, item_id) = linked_state().await;
        let (order_id, _) = open_sent_order(&state, &cookie, &item_id).await;
        open_cash_session(&state, &cookie).await;
        let method_id = payment_method_id(&state).await;
        let csrf = csrf_for(&state, "Ana").await;

        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/bill"),
            &cookie,
            &format!(
                "csrf_token={csrf}&exchange_rate=36\
                 &method_id={method_id}&currency=USD&amount=1&reference=Tarjeta\
                 &method_id={method_id}&currency=VES&amount=150&reference="
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let payments: Vec<(String, i64, i64, i64, i64)> = sqlx::query_as(
            "SELECT currency, amount_minor, base_amount_minor, tendered_minor,
                    change_minor
             FROM payment ORDER BY created_at_ms, id",
        )
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(payments.len(), 2);
        assert_eq!(payments[0], ("USD".to_owned(), 100, 100, 100, 0));
        assert_eq!(payments[1], ("VES".to_owned(), 14_400, 400, 15_000, 600));

        let total: i64 = sqlx::query_scalar("SELECT SUM(base_amount_minor) FROM payment")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(total, 500);
    }

    #[tokio::test]
    async fn cashier_splits_included_iva_on_the_sale_and_receipt() {
        let (state, cookie, item_id) = linked_state().await;
        sqlx::query("UPDATE tax_rate SET is_enabled = 1, rate_basis_points = 1600")
            .execute(&state.pool)
            .await
            .unwrap();
        let (order_id, _) = open_sent_order(&state, &cookie, &item_id).await;
        open_cash_session(&state, &cookie).await;
        let method_id = payment_method_id(&state).await;
        let csrf = csrf_for(&state, "Ana").await;

        let page = get_page(&state, &format!("/app/orders/{order_id}/bill"), &cookie).await;
        let body = body_text(page).await;
        assert!(body.contains("IVA incluido (16%)"));
        assert!(body.contains("4.31 USD"));
        assert!(body.contains("0.69 USD"));

        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/bill"),
            &cookie,
            &format!(
                "csrf_token={csrf}&exchange_rate=36&method_id={method_id}\
                 &currency=USD&amount=5&reference="
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let sale: (i64, i64, i64, Option<String>, Option<i64>) = sqlx::query_as(
            "SELECT subtotal_minor, tax_minor, total_minor, tax_name,
                    tax_rate_basis_points
             FROM sale WHERE order_id = ?1",
        )
        .bind(&order_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(sale, (431, 69, 500, Some("IVA".to_owned()), Some(1600)));

        let receipt = body_text(get_page(&state, &location(&response), &cookie).await).await;
        assert!(receipt.contains("IVA incluido (16%)"));
        assert!(receipt.contains("4.31 USD"));
        assert!(receipt.contains("0.69 USD"));
    }

    #[tokio::test]
    async fn reports_show_sales_payments_and_cash_closures() {
        let (state, cookie, item_id) = linked_state().await;
        let (order_id, _) = open_sent_order(&state, &cookie, &item_id).await;
        open_cash_session(&state, &cookie).await;
        let method_id = payment_method_id(&state).await;
        let csrf = csrf_for(&state, "Ana").await;

        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/bill"),
            &cookie,
            &format!(
                "csrf_token={csrf}&exchange_rate=36&method_id={method_id}\
                 &currency=USD&amount=6&reference="
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let close = post_form(
            &state,
            "/app/cash/close",
            &cookie,
            &format!("csrf_token={csrf}&counted_base=105&counted_secondary=3600&notes=Cierre"),
        )
        .await;
        assert_eq!(close.status(), StatusCode::SEE_OTHER);

        let report = get_page(&state, "/app/reports", &cookie).await;
        assert_eq!(report.status(), StatusCode::OK);
        let body = body_text(report).await;
        assert!(body.contains("Ventas del día"));
        assert!(body.contains("V-0001"));
        assert!(body.contains("Efectivo"));
        assert!(body.contains("Cierres de caja"));
        assert!(body.contains("Cerrada"));
        assert!(body.contains("105.00 USD"));
        assert!(body.contains("3600.00 VES"));

        let csv = get_page(&state, "/app/reports.csv", &cookie).await;
        assert_eq!(csv.status(), StatusCode::OK);
        assert_eq!(
            csv.headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "text/csv; charset=utf-8"
        );
        let body = body_text(csv).await;
        assert!(body.starts_with("\u{feff}numero,fecha,codigo,tipo,estado"));
        assert!(body.contains("V-0001"));
        assert!(body.contains("5.00"));
    }

    #[tokio::test]
    async fn reports_reject_an_invalid_date() {
        let (state, cookie, _) = linked_state().await;

        let response = get_page(&state, "/app/reports?date=not-a-date", &cookie).await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn admin_creates_categories_and_menu_items() {
        let (state, cookie, _) = linked_state().await;
        let csrf = csrf_for(&state, "Ana").await;

        let page = get_page(&state, "/app/admin", &cookie).await;
        assert_eq!(page.status(), StatusCode::OK);
        let body = body_text(page).await;
        assert!(body.contains("Configuración"));
        assert!(body.contains("Café Central"));
        assert!(body.contains("Desayunos"));
        assert!(body.contains("Arepa"));

        let response = post_form(
            &state,
            "/app/admin/categories",
            &cookie,
            &format!("csrf_token={csrf}&id=&name=Cenas&sort_order=2&is_active=true"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let category_id: String =
            sqlx::query_scalar("SELECT id FROM menu_category WHERE name = 'Cenas'")
                .fetch_one(&state.pool)
                .await
                .unwrap();

        let response = post_form(
            &state,
            "/app/admin/items",
            &cookie,
            &format!(
                "csrf_token={csrf}&id=&name=Pasta&description=Con+tomate\
                 &category_id={category_id}&price=7.50&sort_order=1\
                 &is_active=true&is_available=true"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let item: (i64, String, bool) = sqlx::query_as(
            "SELECT price_minor, category_id, is_available
             FROM menu_item WHERE name = 'Pasta'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(item.0, 750);
        assert_eq!(item.1, category_id);
        assert!(item.2);

        let page = body_text(get_page(&state, "/app/admin", &cookie).await).await;
        assert!(page.contains("Cenas"));
        assert!(page.contains("Pasta"));
        assert!(page.contains("7.50 USD"));
    }

    #[tokio::test]
    async fn admin_saves_business_exchange_rate_and_iva() {
        let (state, cookie, _) = linked_state().await;
        let csrf = csrf_for(&state, "Ana").await;

        let response = post_form(
            &state,
            "/app/admin/business",
            &cookie,
            &format!(
                "csrf_token={csrf}&name=Café+Central&legal_name=Café+C.A.\
                 &tax_id=J-123&exchange_rate=40&tax_name=IVA&tax_rate=16\
                 &tax_enabled=true"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let name: String = sqlx::query_scalar("SELECT name FROM business LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(name, "Café Central");
        let rate: String = sqlx::query_scalar(
            "SELECT value FROM business_setting WHERE key = 'exchange_rate_micros'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(rate, "40000000");
        let tax: (i64, bool) =
            sqlx::query_as("SELECT rate_basis_points, is_enabled FROM tax_rate LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(tax, (1_600, true));
    }

    #[tokio::test]
    async fn admin_creates_users_and_guards_the_last_admin() {
        let (state, cookie, _) = linked_state().await;
        let csrf = csrf_for(&state, "Ana").await;

        let response = post_form(
            &state,
            "/app/admin/users",
            &cookie,
            &format!(
                "csrf_token={csrf}&id=&display_name=Pedro&role=waiter\
                 &pin=5555&is_active=true"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let user: (String, bool, String) = sqlx::query_as(
            "SELECT role, is_active, pin_hash FROM app_user WHERE display_name = 'Pedro'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(user.0, "waiter");
        assert!(user.1);
        assert!(user.2.starts_with("$argon2"));

        let response = post_form(
            &state,
            "/app/admin/users",
            &cookie,
            &format!(
                "csrf_token={csrf}&id=&display_name=Pedro&role=cashier\
                 &pin=4444&is_active=true"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let ana_id: String =
            sqlx::query_scalar("SELECT id FROM app_user WHERE display_name = 'Ana'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let response = post_form(
            &state,
            "/app/admin/users",
            &cookie,
            &format!(
                "csrf_token={csrf}&id={ana_id}&display_name=Ana&role=waiter\
                 &pin=&is_active=true"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let role: String = sqlx::query_scalar("SELECT role FROM app_user WHERE id = ?1")
            .bind(&ana_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(role, "admin");
    }

    #[tokio::test]
    async fn admin_creates_tables_and_payment_methods() {
        let (state, cookie, _) = linked_state().await;
        let csrf = csrf_for(&state, "Ana").await;

        let response = post_form(
            &state,
            "/app/admin/tables",
            &cookie,
            &format!("csrf_token={csrf}&id=&name=Terraza+1&zone=Terraza&is_active=true"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let table: (String, String, bool) = sqlx::query_as(
            "SELECT name, zone, is_active FROM dining_table WHERE name = 'Terraza 1'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(table.0, "Terraza 1");
        assert_eq!(table.1, "Terraza");
        assert!(table.2);

        let response = post_form(
            &state,
            "/app/admin/payment-methods",
            &cookie,
            &format!(
                "csrf_token={csrf}&id=&name=Pago+móvil&kind=mobile_payment\
                 &currency=ves&sort_order=3&is_active=true"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        let method: (String, String, String, i64, bool) = sqlx::query_as(
            "SELECT name, kind, currency, sort_order, is_active
             FROM payment_method WHERE name = 'Pago móvil'",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(method.0, "Pago móvil");
        assert_eq!(method.1, "mobile_payment");
        assert_eq!(method.2, "VES");
        assert_eq!(method.3, 3);
        assert!(method.4);

        let response = post_form(
            &state,
            "/app/admin/payment-methods",
            &cookie,
            &format!(
                "csrf_token={csrf}&id=&name=Pago+móvil&kind=cash\
                 &currency=&sort_order=4&is_active=true"
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn admin_page_is_restricted_to_admins() {
        let (state, _cookie, _) = linked_state().await;
        add_user(&state, "Cajera", "cashier", "1111").await;
        let cashier_cookie = web_login_as(&state, "Cajera", "1111").await;

        let response = get_page(&state, "/app/admin", &cashier_cookie).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn admin_backup_downloads_a_sqlite_file() {
        let state = test_support::file_state().await;
        let snapshot = crate::snapshot::sample_snapshot();
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

        let backup = get_page(&state, "/app/admin/backup", &cookie).await;
        assert_eq!(backup.status(), StatusCode::OK);
        assert_eq!(
            backup
                .headers()
                .get(header::CONTENT_TYPE)
                .unwrap()
                .to_str()
                .unwrap(),
            "application/octet-stream"
        );
        let bytes = backup.into_body().collect().await.unwrap().to_bytes();
        assert!(bytes.starts_with(b"SQLite format 3\0"));
    }

    #[tokio::test]
    async fn cash_payments_require_an_open_cash_session() {
        let (state, cookie, item_id) = linked_state().await;
        let (order_id, _) = open_sent_order(&state, &cookie, &item_id).await;
        let method_id = payment_method_id(&state).await;
        let csrf = csrf_for(&state, "Ana").await;

        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/bill"),
            &cookie,
            &format!(
                "csrf_token={csrf}&exchange_rate=36&method_id={method_id}\
                 &currency=USD&amount=5&reference="
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let sales: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sale")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(sales, 0);
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
        assert!(body.contains("Volver a mesas"));
        assert!(!body.contains("Volver a para llevar"));
        assert!(
            body.contains(
                "<a href=\"/app/tables\" class=\"active\" aria-current=\"page\">Mesas</a>"
            )
        );

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
    async fn order_status_fragment_tracks_the_order() {
        let (state, cookie, item_id) = linked_state().await;
        let (order_id, _) = open_sent_order(&state, &cookie, &item_id).await;

        let response = get_page(&state, &format!("/app/orders/{order_id}/status"), &cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("id=\"order-status\""));
        assert!(body.contains("hx-trigger=\"every 5s\""));
        assert!(body.contains("En curso"));
        assert!(body.contains("Total"));
    }

    #[tokio::test]
    async fn cancel_order_requires_a_reason_and_a_manager_role() {
        let (state, cookie, item_id) = linked_state().await;
        let (order_id, _) = open_sent_order(&state, &cookie, &item_id).await;

        add_user(&state, "Mesonero", "waiter", "2222").await;
        let waiter_cookie = web_login_as(&state, "Mesonero", "2222").await;
        let waiter_csrf = csrf_for(&state, "Mesonero").await;
        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/cancel"),
            &waiter_cookie,
            &format!("csrf_token={waiter_csrf}&reason=Prueba"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let csrf = csrf_for(&state, "Ana").await;
        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/cancel"),
            &cookie,
            &format!("csrf_token={csrf}&reason="),
        )
        .await;
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);

        let response = post_form(
            &state,
            &format!("/app/orders/{order_id}/cancel"),
            &cookie,
            &format!("csrf_token={csrf}&reason=Cliente+se+retiro"),
        )
        .await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(
            response
                .headers()
                .get(header::LOCATION)
                .unwrap()
                .to_str()
                .unwrap(),
            "/app/tables"
        );

        let order: (String, String, bool) = sqlx::query_as(
            "SELECT status, cancel_reason, closed_at_ms IS NOT NULL
             FROM restaurant_order WHERE id = ?1",
        )
        .bind(&order_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(order.0, "cancelled");
        assert_eq!(order.1, "Cliente se retiro");
        assert!(order.2);

        let open_orders: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM restaurant_order
             WHERE status IN ('open', 'active', 'served')",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(open_orders, 0);

        let events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM order_event
             WHERE order_id = ?1 AND kind = 'order_cancelled'",
        )
        .bind(&order_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(events, 1);
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

        let detail = body_text(get_page(&state, &location, &cookie).await).await;
        assert!(detail.contains("Volver a para llevar"));
        assert!(!detail.contains("Volver a mesas"));
        assert!(detail.contains(
            "<a href=\"/app/takeout\" class=\"active\" aria-current=\"page\">Para llevar</a>"
        ));
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
        assert!(body.contains("viewport-fit=cover"));
        assert!(body.contains("rel=\"manifest\""));
        assert!(body.contains("apple-mobile-web-app-capable"));
        assert!(body.contains("apple-touch-icon"));

        let response = router(state)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");
    }

    #[tokio::test]
    async fn mobile_web_app_metadata_is_served() {
        let state = test_support::state().await;

        let cases = [
            (
                "/static/manifest.webmanifest",
                "application/manifest+json",
                Some("\"start_url\": \"/app\""),
            ),
            ("/static/icon.svg", "image/svg+xml", Some("<svg")),
            ("/static/icon-192.png", "image/png", None),
            ("/static/icon-512.png", "image/png", None),
            ("/static/apple-touch-icon.png", "image/png", None),
        ];

        for (uri, content_type, marker) in cases {
            let response = router(state.clone())
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::OK, "{uri}");
            assert_eq!(
                response
                    .headers()
                    .get(header::CONTENT_TYPE)
                    .unwrap()
                    .to_str()
                    .unwrap(),
                content_type,
                "{uri}"
            );

            if let Some(marker) = marker {
                assert!(body_text(response).await.contains(marker), "{uri}");
            }
        }
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
    async fn login_is_throttled_after_repeated_failures() {
        let state = test_support::state().await;

        for _ in 0..auth::LOGIN_MAX_FAILURES {
            let response = router(state.clone())
                .oneshot(form_request("/login", "user=Admin&pin=0000"))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let response = router(state)
            .oneshot(form_request("/login", "user=Admin&pin=1234"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after: i64 = response
            .headers()
            .get(header::RETRY_AFTER)
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        assert!((1..=auth::LOGIN_FIRST_LOCK_MS / 1000).contains(&retry_after));
        let body = body_text(response).await;
        assert!(body.contains("Demasiados intentos"));
        assert!(body.contains("1 minuto"));
    }

    #[tokio::test]
    async fn deactivated_users_lose_web_access_immediately() {
        let state = test_support::state().await;
        let cookie = web_login(&state).await;

        sqlx::query("UPDATE app_user SET is_active = 0")
            .execute(&state.pool)
            .await
            .unwrap();

        let response = get_page(&state, "/app", &cookie).await;
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");
    }

    #[tokio::test]
    async fn page_permissions_and_navigation_follow_the_role() {
        let (state, _admin_cookie, _item_id) = linked_state().await;
        add_user(&state, "Cocina", "kitchen", "1357").await;
        add_user(&state, "Mesonero", "waiter", "2468").await;

        let kitchen_cookie = web_login_as(&state, "Cocina", "1357").await;
        let waiter_cookie = web_login_as(&state, "Mesonero", "2468").await;

        let response = get_page(&state, "/app/tables", &kitchen_cookie).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = get_page(&state, "/app/takeout", &kitchen_cookie).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = get_page(&state, "/app/kitchen", &kitchen_cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("Cocina"));
        assert!(!body.contains("href=\"/app/tables\""));
        assert!(!body.contains("href=\"/app/takeout\""));

        let response = get_page(&state, "/app/kitchen", &waiter_cookie).await;
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let response = get_page(&state, "/app/tables", &waiter_cookie).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_text(response).await;
        assert!(body.contains("href=\"/app/tables\""));
        assert!(body.contains("href=\"/app/takeout\""));
        assert!(!body.contains("href=\"/app/kitchen\""));
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
