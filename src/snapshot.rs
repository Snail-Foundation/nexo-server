//! Desktop linking and snapshot import.
//!
//! Linking follows the project rule: an empty server accepts a snapshot from
//! the desktop. A server that already has data is never overwritten silently;
//! the desktop must choose the source explicitly in a later flow.

use nexo_core::{DeviceId, Uuid};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use crate::auth;
use crate::error::AppError;
use crate::time::now_ms;

pub const SERVER_DEVICE_KEY: &str = "server_device_id";
pub const SNAPSHOT_IMPORTED_KEY: &str = "snapshot_imported_at_ms";

#[derive(Debug, Deserialize)]
pub struct LinkRequest {
    pub server_user: String,
    pub server_pin: String,
    pub snapshot: Snapshot,
}

#[derive(Debug, Deserialize)]
pub struct Snapshot {
    pub business: SnapshotBusiness,
    pub branch: SnapshotBranch,
    pub device: SnapshotDevice,
    pub users: Vec<SnapshotUser>,
    pub categories: Vec<SnapshotCategory>,
    pub items: Vec<SnapshotItem>,
    pub tables: Vec<SnapshotTable>,
    pub tax_rate: Option<SnapshotTaxRate>,
    pub payment_methods: Vec<SnapshotPaymentMethod>,
    #[serde(default)]
    pub settings: Vec<SnapshotSetting>,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotBusiness {
    pub id: String,
    pub name: String,
    pub legal_name: Option<String>,
    pub tax_id: Option<String>,
    pub base_currency: String,
    pub locale: String,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotBranch {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotDevice {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotUser {
    pub id: String,
    pub display_name: String,
    pub role: String,
    pub pin_hash: String,
    pub is_active: bool,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotCategory {
    pub id: String,
    pub name: String,
    pub sort_order: i64,
    pub is_active: bool,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotItem {
    pub id: String,
    pub category_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub price_minor: i64,
    pub currency: String,
    pub is_active: bool,
    pub is_available: bool,
    pub sort_order: i64,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotTable {
    pub id: String,
    pub name: String,
    pub zone: Option<String>,
    pub is_active: bool,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotTaxRate {
    pub id: String,
    pub name: String,
    pub rate_basis_points: i64,
    pub enabled: bool,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotPaymentMethod {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub currency: Option<String>,
    pub is_active: bool,
    pub sort_order: i64,
}

#[derive(Debug, Deserialize)]
pub struct SnapshotSetting {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Serialize)]
pub struct LinkResponse {
    pub device_id: String,
    pub device_token: String,
    pub business_id: String,
    pub branch_id: String,
    pub last_seq: i64,
}

/// Imports a desktop snapshot into an empty server and issues a device token.
pub async fn link(pool: &SqlitePool, request: LinkRequest) -> Result<LinkResponse, AppError> {
    let Some(admin) =
        auth::authenticate_throttled(pool, &request.server_user, &request.server_pin).await?
    else {
        return Err(AppError::Unauthorized);
    };

    if admin.role != "admin" {
        return Err(AppError::Forbidden);
    }

    if server_has_data(pool).await? {
        return Err(AppError::Conflict(
            "the server already has data; choose the synchronization source explicitly".to_owned(),
        ));
    }

    let snapshot = validate(request.snapshot)?;
    let device_token = auth::random_token();
    let token_hash = auth::hash_token(&device_token);
    let now = now_ms();

    let business_id = snapshot.business.id.clone();
    let branch_id = snapshot.branch.id.clone();
    let device_id = snapshot.device.id.clone();

    let mut transaction = pool.begin().await?;

    // The seed business and its rows are replaced by the desktop snapshot.
    // The operation log stays untouched and must be empty at this point.
    sqlx::query("DELETE FROM business")
        .execute(&mut *transaction)
        .await?;

    sqlx::query(
        "INSERT INTO business (
            id, name, legal_name, tax_id, base_currency, locale,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
    )
    .bind(&business_id)
    .bind(&snapshot.business.name)
    .bind(&snapshot.business.legal_name)
    .bind(&snapshot.business.tax_id)
    .bind(&snapshot.business.base_currency)
    .bind(&snapshot.business.locale)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO branch (
            id, business_id, name, is_default, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, 1, ?4, ?4)",
    )
    .bind(&branch_id)
    .bind(&business_id)
    .bind(&snapshot.branch.name)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    for user in &snapshot.users {
        sqlx::query(
            "INSERT INTO app_user (
                id, business_id, display_name, role, pin_hash, is_active,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        )
        .bind(&user.id)
        .bind(&business_id)
        .bind(&user.display_name)
        .bind(&user.role)
        .bind(&user.pin_hash)
        .bind(user.is_active)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }

    if let Some(tax_rate) = &snapshot.tax_rate {
        sqlx::query(
            "INSERT INTO tax_rate (
                id, business_id, name, rate_basis_points, is_enabled,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        )
        .bind(&tax_rate.id)
        .bind(&business_id)
        .bind(&tax_rate.name)
        .bind(tax_rate.rate_basis_points)
        .bind(tax_rate.enabled)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }

    for method in &snapshot.payment_methods {
        sqlx::query(
            "INSERT INTO payment_method (
                id, business_id, name, kind, currency, is_active, sort_order,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
        )
        .bind(&method.id)
        .bind(&business_id)
        .bind(&method.name)
        .bind(&method.kind)
        .bind(&method.currency)
        .bind(method.is_active)
        .bind(method.sort_order)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }

    for category in &snapshot.categories {
        sqlx::query(
            "INSERT INTO menu_category (
                id, business_id, name, sort_order, is_active,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        )
        .bind(&category.id)
        .bind(&business_id)
        .bind(&category.name)
        .bind(category.sort_order)
        .bind(category.is_active)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }

    for item in &snapshot.items {
        sqlx::query(
            "INSERT INTO menu_item (
                id, business_id, category_id, name, description, price_minor,
                currency, is_active, is_available, sort_order,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)",
        )
        .bind(&item.id)
        .bind(&business_id)
        .bind(&item.category_id)
        .bind(&item.name)
        .bind(&item.description)
        .bind(item.price_minor)
        .bind(&item.currency)
        .bind(item.is_active)
        .bind(item.is_available)
        .bind(item.sort_order)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }

    for table in &snapshot.tables {
        sqlx::query(
            "INSERT INTO dining_table (
                id, business_id, branch_id, name, zone, is_active,
                created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        )
        .bind(&table.id)
        .bind(&business_id)
        .bind(&branch_id)
        .bind(&table.name)
        .bind(&table.zone)
        .bind(table.is_active)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }

    for setting in &snapshot.settings {
        sqlx::query(
            "INSERT INTO business_setting (business_id, key, value, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .bind(&business_id)
        .bind(&setting.key)
        .bind(&setting.value)
        .bind(now)
        .execute(&mut *transaction)
        .await?;
    }

    sqlx::query(
        "INSERT INTO device (
            id, business_id, branch_id, name, kind, is_master, token_hash,
            registered_at_ms
         ) VALUES (?1, ?2, ?3, ?4, 'desktop', 1, ?5, ?6)",
    )
    .bind(&device_id)
    .bind(&business_id)
    .bind(&branch_id)
    .bind(&snapshot.device.name)
    .bind(token_hash)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    let server_device_id = DeviceId::new();
    sqlx::query(
        "INSERT INTO device (
            id, business_id, branch_id, name, kind, is_master, registered_at_ms
         ) VALUES (?1, ?2, ?3, 'Servidor', 'server', 0, ?4)",
    )
    .bind(server_device_id.to_string())
    .bind(&business_id)
    .bind(&branch_id)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO server_setting (business_id, key, value, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4), (?1, ?5, ?6, ?4)",
    )
    .bind(&business_id)
    .bind(SERVER_DEVICE_KEY)
    .bind(server_device_id.to_string())
    .bind(now)
    .bind(SNAPSHOT_IMPORTED_KEY)
    .bind(now.to_string())
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;

    Ok(LinkResponse {
        device_id,
        device_token,
        business_id,
        branch_id,
        last_seq: 0,
    })
}

/// Returns true when the server already has business state or operations.
pub async fn server_has_data(pool: &SqlitePool) -> Result<bool, AppError> {
    let operations: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operation_log")
        .fetch_one(pool)
        .await?;
    if operations > 0 {
        return Ok(true);
    }

    let settings: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM business_setting")
        .fetch_one(pool)
        .await?;
    if settings > 0 {
        return Ok(true);
    }

    let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_item")
        .fetch_one(pool)
        .await?;
    if items > 0 {
        return Ok(true);
    }

    let tables: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM dining_table")
        .fetch_one(pool)
        .await?;

    Ok(tables > 0)
}

/// Returns the server device ID used for operations written by the web app.
pub async fn server_device_id(pool: &SqlitePool) -> Result<Option<String>, AppError> {
    let business_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM business ORDER BY created_at_ms LIMIT 1")
            .fetch_optional(pool)
            .await?;

    let Some(business_id) = business_id else {
        return Ok(None);
    };

    let from_setting: Option<String> =
        sqlx::query_scalar("SELECT value FROM server_setting WHERE business_id = ?1 AND key = ?2")
            .bind(&business_id)
            .bind(SERVER_DEVICE_KEY)
            .fetch_optional(pool)
            .await?;

    if from_setting.is_some() {
        return Ok(from_setting);
    }

    Ok(sqlx::query_scalar(
        "SELECT id FROM device
         WHERE business_id = ?1 AND kind = 'server'
         ORDER BY registered_at_ms
         LIMIT 1",
    )
    .bind(business_id)
    .fetch_optional(pool)
    .await?)
}

fn validate(snapshot: Snapshot) -> Result<Snapshot, AppError> {
    validate_id(&snapshot.business.id, "business id")?;
    validate_id(&snapshot.branch.id, "branch id")?;
    validate_id(&snapshot.device.id, "device id")?;

    if snapshot.business.name.trim().is_empty() {
        return Err(invalid("business name is required"));
    }
    if snapshot.business.base_currency.len() != 3 {
        return Err(invalid("base currency must have three letters"));
    }
    if snapshot.business.locale != "es" && snapshot.business.locale != "en" {
        return Err(invalid("locale must be es or en"));
    }
    if snapshot.branch.name.trim().is_empty() {
        return Err(invalid("branch name is required"));
    }
    if snapshot.device.name.trim().is_empty() {
        return Err(invalid("device name is required"));
    }

    if snapshot.users.is_empty() {
        return Err(invalid("at least one user is required"));
    }
    for user in &snapshot.users {
        validate_id(&user.id, "user id")?;
        if user.display_name.trim().is_empty() {
            return Err(invalid("user name is required"));
        }
        if !matches!(
            user.role.as_str(),
            "admin" | "cashier" | "waiter" | "kitchen"
        ) {
            return Err(invalid("invalid user role"));
        }
        if !user.pin_hash.starts_with("$argon2") {
            return Err(invalid("user PIN must be an Argon2 verifier"));
        }
    }

    let mut category_ids = Vec::with_capacity(snapshot.categories.len());
    for category in &snapshot.categories {
        validate_id(&category.id, "category id")?;
        if category.name.trim().is_empty() {
            return Err(invalid("category name is required"));
        }
        category_ids.push(category.id.as_str());
    }

    for item in &snapshot.items {
        validate_id(&item.id, "item id")?;
        if item.name.trim().is_empty() {
            return Err(invalid("item name is required"));
        }
        if item.price_minor < 0 {
            return Err(invalid("item price cannot be negative"));
        }
        if item.currency.len() != 3 {
            return Err(invalid("item currency must have three letters"));
        }
        if let Some(category_id) = &item.category_id {
            validate_id(category_id, "item category id")?;
            if !category_ids.contains(&category_id.as_str()) {
                return Err(invalid("item references an unknown category"));
            }
        }
    }

    for table in &snapshot.tables {
        validate_id(&table.id, "table id")?;
        if table.name.trim().is_empty() {
            return Err(invalid("table name is required"));
        }
    }

    if let Some(tax_rate) = &snapshot.tax_rate {
        validate_id(&tax_rate.id, "tax rate id")?;
        if !(0..=10_000).contains(&tax_rate.rate_basis_points) {
            return Err(invalid("tax rate must be between 0 and 10000"));
        }
    }

    for method in &snapshot.payment_methods {
        validate_id(&method.id, "payment method id")?;
        if method.name.trim().is_empty() {
            return Err(invalid("payment method name is required"));
        }
        if !matches!(
            method.kind.as_str(),
            "cash" | "card" | "transfer" | "mobile_payment" | "other"
        ) {
            return Err(invalid("invalid payment method kind"));
        }
        if method
            .currency
            .as_ref()
            .is_some_and(|currency| currency.len() != 3)
        {
            return Err(invalid("payment method currency must have three letters"));
        }
    }

    for setting in &snapshot.settings {
        if setting.key.trim().is_empty() {
            return Err(invalid("setting key is required"));
        }
    }

    Ok(snapshot)
}

fn validate_id(value: &str, field: &str) -> Result<(), AppError> {
    value
        .parse::<Uuid>()
        .map(|_| ())
        .map_err(|_| invalid(&format!("invalid {field}")))
}

fn invalid(message: &str) -> AppError {
    AppError::Validation(message.to_owned())
}

#[cfg(test)]
pub(crate) fn sample_snapshot() -> Snapshot {
    use nexo_core::{DiningTableId, MenuCategoryId, MenuItemId, PaymentMethodId, TaxRateId};

    let business_id = nexo_core::BusinessId::new();
    let branch_id = nexo_core::BranchId::new();
    let device_id = nexo_core::DeviceId::new();
    let user_id = nexo_core::UserId::new();
    let category_id = MenuCategoryId::new();
    let item_id = MenuItemId::new();
    let table_id = DiningTableId::new();
    let tax_rate_id = TaxRateId::new();
    let payment_method_id = PaymentMethodId::new();

    Snapshot {
        business: SnapshotBusiness {
            id: business_id.to_string(),
            name: "Café Central".to_owned(),
            legal_name: None,
            tax_id: None,
            base_currency: "USD".to_owned(),
            locale: "es".to_owned(),
        },
        branch: SnapshotBranch {
            id: branch_id.to_string(),
            name: "Principal".to_owned(),
        },
        device: SnapshotDevice {
            id: device_id.to_string(),
            name: "Caja 1".to_owned(),
        },
        users: vec![SnapshotUser {
            id: user_id.to_string(),
            display_name: "Ana".to_owned(),
            role: "admin".to_owned(),
            pin_hash: crate::auth::hash_pin("2468").unwrap(),
            is_active: true,
        }],
        categories: vec![SnapshotCategory {
            id: category_id.to_string(),
            name: "Desayunos".to_owned(),
            sort_order: 0,
            is_active: true,
        }],
        items: vec![SnapshotItem {
            id: item_id.to_string(),
            category_id: Some(category_id.to_string()),
            name: "Arepa".to_owned(),
            description: Some("Con queso".to_owned()),
            price_minor: 250,
            currency: "USD".to_owned(),
            is_active: true,
            is_available: true,
            sort_order: 0,
        }],
        tables: vec![SnapshotTable {
            id: table_id.to_string(),
            name: "Mesa 1".to_owned(),
            zone: Some("Salón".to_owned()),
            is_active: true,
        }],
        tax_rate: Some(SnapshotTaxRate {
            id: tax_rate_id.to_string(),
            name: "IVA".to_owned(),
            rate_basis_points: 1_600,
            enabled: false,
        }),
        payment_methods: vec![SnapshotPaymentMethod {
            id: payment_method_id.to_string(),
            name: "Efectivo".to_owned(),
            kind: "cash".to_owned(),
            currency: None,
            is_active: true,
            sort_order: 0,
        }],
        settings: vec![SnapshotSetting {
            key: "exchange_rate_micros".to_owned(),
            value: "36000000".to_owned(),
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support;

    #[tokio::test]
    async fn linking_imports_the_snapshot_and_issues_a_device_token() {
        let state = test_support::state().await;
        let snapshot = sample_snapshot();
        let device_id = snapshot.device.id.clone();
        let item_id = snapshot.items[0].id.clone();

        let response = link(
            &state.pool,
            LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.device_id, device_id);
        assert!(!response.device_token.is_empty());
        assert_eq!(response.last_seq, 0);

        let business_name: String =
            sqlx::query_scalar("SELECT name FROM business ORDER BY created_at_ms LIMIT 1")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(business_name, "Café Central");

        let stored_item: String = sqlx::query_scalar("SELECT name FROM menu_item WHERE id = ?1")
            .bind(&item_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(stored_item, "Arepa");

        let stored_hash: String = sqlx::query_scalar("SELECT token_hash FROM device WHERE id = ?1")
            .bind(&device_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(stored_hash, auth::hash_token(&response.device_token));

        let server_devices: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM device WHERE kind = 'server'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(server_devices, 1);
    }

    #[tokio::test]
    async fn a_server_with_data_rejects_a_second_link() {
        let state = test_support::state().await;

        link(
            &state.pool,
            LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot: sample_snapshot(),
            },
        )
        .await
        .unwrap();

        let result = link(
            &state.pool,
            LinkRequest {
                server_user: "Ana".to_owned(),
                server_pin: "2468".to_owned(),
                snapshot: sample_snapshot(),
            },
        )
        .await;

        assert!(matches!(result, Err(AppError::Conflict(_))));
    }

    #[tokio::test]
    async fn linking_requires_the_server_admin_pin() {
        let state = test_support::state().await;

        let result = link(
            &state.pool,
            LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "0000".to_owned(),
                snapshot: sample_snapshot(),
            },
        )
        .await;

        assert!(matches!(result, Err(AppError::Unauthorized)));
    }

    #[tokio::test]
    async fn linking_shares_login_throttling() {
        let state = test_support::state().await;

        for _ in 0..auth::LOGIN_MAX_FAILURES {
            let result = link(
                &state.pool,
                LinkRequest {
                    server_user: "Admin".to_owned(),
                    server_pin: "0000".to_owned(),
                    snapshot: sample_snapshot(),
                },
            )
            .await;

            assert!(matches!(result, Err(AppError::Unauthorized)));
        }

        let result = link(
            &state.pool,
            LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot: sample_snapshot(),
            },
        )
        .await;

        assert!(matches!(result, Err(AppError::TooManyAttempts { .. })));
    }
}
