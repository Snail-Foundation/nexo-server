//! Server projections of the operation log.
//!
//! Projections are a deterministic fold in server sequence order. Phase 2
//! covers master data; order, sale, and cash projections arrive with their
//! phases. An operation whose payload is invalid is stored in the log and
//! recorded as an anomaly, but it never changes the projection.

use nexo_core::{Operation, OperationKind};
use serde_json::Value;
use sqlx::{Row, Sqlite, Transaction};

use crate::error::AppError;

/// Result of folding one operation into the projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectionOutcome {
    /// The projection changed.
    Applied,
    /// The operation belongs to a later phase and is not projected yet.
    Ignored,
    /// The operation was stored but not projected. The caller records an
    /// anomaly with this reason.
    Skipped(String),
}

/// Applies one operation to the master-data projection.
pub async fn apply(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    match operation.kind {
        OperationKind::BusinessConfigured => business_configured(transaction, operation).await,
        OperationKind::SettingChanged => setting_changed(transaction, operation).await,
        OperationKind::TaxRateChanged => tax_rate_changed(transaction, operation).await,
        OperationKind::PaymentMethodUpserted => {
            payment_method_upserted(transaction, operation).await
        }
        OperationKind::MenuCategoryUpserted => menu_category_upserted(transaction, operation).await,
        OperationKind::MenuItemUpserted => menu_item_upserted(transaction, operation).await,
        OperationKind::MenuItemAvailabilityChanged => {
            menu_item_availability_changed(transaction, operation).await
        }
        OperationKind::DiningTableUpserted => dining_table_upserted(transaction, operation).await,
        OperationKind::UserUpserted => user_upserted(transaction, operation).await,
        _ => Ok(ProjectionOutcome::Ignored),
    }
}

async fn business_configured(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(name) = text(payload, "name") else {
        return Ok(invalid("business_configured requires a name"));
    };
    let Some(base_currency) = text(payload, "base_currency") else {
        return Ok(invalid("business_configured requires a base currency"));
    };
    if base_currency.len() != 3 {
        return Ok(invalid("base_currency must have three letters"));
    }
    let locale = text(payload, "locale").unwrap_or_else(|| "es".to_owned());
    if locale != "es" && locale != "en" {
        return Ok(invalid("locale must be es or en"));
    }

    let legal_name = optional_text(payload, "legal_name");
    let tax_id = optional_text(payload, "tax_id");
    let at = operation.created_at_ms;

    match business_id(transaction).await? {
        Some(id) => {
            sqlx::query(
                "UPDATE business
                 SET name = ?2, legal_name = ?3, tax_id = ?4,
                     base_currency = ?5, locale = ?6, updated_at_ms = ?7
                 WHERE id = ?1",
            )
            .bind(id)
            .bind(name)
            .bind(legal_name)
            .bind(tax_id)
            .bind(base_currency)
            .bind(locale)
            .bind(at)
            .execute(&mut **transaction)
            .await?;
        }
        None => {
            sqlx::query(
                "INSERT INTO business (
                    id, name, legal_name, tax_id, base_currency, locale,
                    created_at_ms, updated_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
            )
            .bind(operation.entity_id.to_string())
            .bind(name)
            .bind(legal_name)
            .bind(tax_id)
            .bind(base_currency)
            .bind(locale)
            .bind(at)
            .execute(&mut **transaction)
            .await?;
        }
    }

    Ok(ProjectionOutcome::Applied)
}

async fn setting_changed(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let Some(key) = text(&operation.payload, "key") else {
        return Ok(invalid("setting_changed requires a key"));
    };
    let Some(value) = text(&operation.payload, "value") else {
        return Ok(invalid("setting_changed requires a value"));
    };
    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("setting_changed requires a business"));
    };

    sqlx::query(
        "INSERT INTO business_setting (business_id, key, value, updated_at_ms)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (business_id, key)
         DO UPDATE SET value = excluded.value, updated_at_ms = excluded.updated_at_ms",
    )
    .bind(business_id)
    .bind(key)
    .bind(value)
    .bind(operation.created_at_ms)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn tax_rate_changed(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(name) = text(payload, "name") else {
        return Ok(invalid("tax_rate_changed requires a name"));
    };
    let Some(rate) = integer(payload, "rate_basis_points") else {
        return Ok(invalid("tax_rate_changed requires a rate"));
    };
    if !(0..=10_000).contains(&rate) {
        return Ok(invalid("rate_basis_points must be between 0 and 10000"));
    }
    let Some(enabled) = boolean(payload, "enabled") else {
        return Ok(invalid("tax_rate_changed requires enabled"));
    };
    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("tax_rate_changed requires a business"));
    };
    let at = operation.created_at_ms;

    sqlx::query(
        "INSERT INTO tax_rate (
            id, business_id, name, rate_basis_points, is_enabled,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT (id) DO UPDATE SET
            name = excluded.name,
            rate_basis_points = excluded.rate_basis_points,
            is_enabled = excluded.is_enabled,
            updated_at_ms = excluded.updated_at_ms",
    )
    .bind(operation.entity_id.to_string())
    .bind(business_id)
    .bind(name)
    .bind(rate)
    .bind(enabled)
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn payment_method_upserted(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(name) = text(payload, "name") else {
        return Ok(invalid("payment_method_upserted requires a name"));
    };
    let Some(kind) = text(payload, "kind") else {
        return Ok(invalid("payment_method_upserted requires a kind"));
    };
    if !matches!(
        kind.as_str(),
        "cash" | "card" | "transfer" | "mobile_payment" | "other"
    ) {
        return Ok(invalid("invalid payment method kind"));
    }
    let currency = optional_text(payload, "currency");
    if currency.as_ref().is_some_and(|value| value.len() != 3) {
        return Ok(invalid("currency must have three letters"));
    }
    let Some(is_active) = boolean(payload, "is_active") else {
        return Ok(invalid("payment_method_upserted requires is_active"));
    };
    let sort_order = integer(payload, "sort_order").unwrap_or(0);
    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("payment_method_upserted requires a business"));
    };
    let at = operation.created_at_ms;

    sqlx::query(
        "INSERT INTO payment_method (
            id, business_id, name, kind, currency, is_active, sort_order,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT (id) DO UPDATE SET
            name = excluded.name,
            kind = excluded.kind,
            currency = excluded.currency,
            is_active = excluded.is_active,
            sort_order = excluded.sort_order,
            updated_at_ms = excluded.updated_at_ms",
    )
    .bind(operation.entity_id.to_string())
    .bind(business_id)
    .bind(name)
    .bind(kind)
    .bind(currency)
    .bind(is_active)
    .bind(sort_order)
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn menu_category_upserted(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(name) = text(payload, "name") else {
        return Ok(invalid("menu_category_upserted requires a name"));
    };
    let sort_order = integer(payload, "sort_order").unwrap_or(0);
    let Some(is_active) = boolean(payload, "is_active") else {
        return Ok(invalid("menu_category_upserted requires is_active"));
    };
    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("menu_category_upserted requires a business"));
    };
    let at = operation.created_at_ms;

    sqlx::query(
        "INSERT INTO menu_category (
            id, business_id, name, sort_order, is_active,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)
         ON CONFLICT (id) DO UPDATE SET
            name = excluded.name,
            sort_order = excluded.sort_order,
            is_active = excluded.is_active,
            updated_at_ms = excluded.updated_at_ms",
    )
    .bind(operation.entity_id.to_string())
    .bind(business_id)
    .bind(name)
    .bind(sort_order)
    .bind(is_active)
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn menu_item_upserted(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(name) = text(payload, "name") else {
        return Ok(invalid("menu_item_upserted requires a name"));
    };
    let Some(price_minor) = integer(payload, "price_minor") else {
        return Ok(invalid("menu_item_upserted requires a price"));
    };
    if price_minor < 0 {
        return Ok(invalid("price_minor cannot be negative"));
    }
    let Some(currency) = text(payload, "currency") else {
        return Ok(invalid("menu_item_upserted requires a currency"));
    };
    if currency.len() != 3 {
        return Ok(invalid("currency must have three letters"));
    }
    let Some(is_active) = boolean(payload, "is_active") else {
        return Ok(invalid("menu_item_upserted requires is_active"));
    };
    let Some(is_available) = boolean(payload, "is_available") else {
        return Ok(invalid("menu_item_upserted requires is_available"));
    };
    let sort_order = integer(payload, "sort_order").unwrap_or(0);
    let description = optional_text(payload, "description");
    let category_id = optional_text(payload, "category_id");

    if let Some(category_id) = &category_id {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM menu_category WHERE id = ?1)")
                .bind(category_id)
                .fetch_one(&mut **transaction)
                .await?;
        if !exists {
            return Ok(invalid("the menu category does not exist"));
        }
    }

    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("menu_item_upserted requires a business"));
    };
    let at = operation.created_at_ms;

    sqlx::query(
        "INSERT INTO menu_item (
            id, business_id, category_id, name, description, price_minor,
            currency, is_active, is_available, sort_order,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11)
         ON CONFLICT (id) DO UPDATE SET
            category_id = excluded.category_id,
            name = excluded.name,
            description = excluded.description,
            price_minor = excluded.price_minor,
            currency = excluded.currency,
            is_active = excluded.is_active,
            is_available = excluded.is_available,
            sort_order = excluded.sort_order,
            updated_at_ms = excluded.updated_at_ms",
    )
    .bind(operation.entity_id.to_string())
    .bind(business_id)
    .bind(category_id)
    .bind(name)
    .bind(description)
    .bind(price_minor)
    .bind(currency)
    .bind(is_active)
    .bind(is_available)
    .bind(sort_order)
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn menu_item_availability_changed(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let Some(is_available) = boolean(&operation.payload, "is_available") else {
        return Ok(invalid(
            "menu_item_availability_changed requires is_available",
        ));
    };

    let updated = sqlx::query(
        "UPDATE menu_item
         SET is_available = ?2, updated_at_ms = ?3
         WHERE id = ?1",
    )
    .bind(operation.entity_id.to_string())
    .bind(is_available)
    .bind(operation.created_at_ms)
    .execute(&mut **transaction)
    .await?;

    if updated.rows_affected() == 0 {
        return Ok(invalid("the menu item does not exist"));
    }

    Ok(ProjectionOutcome::Applied)
}

async fn dining_table_upserted(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(name) = text(payload, "name") else {
        return Ok(invalid("dining_table_upserted requires a name"));
    };
    let zone = optional_text(payload, "zone");
    let Some(is_active) = boolean(payload, "is_active") else {
        return Ok(invalid("dining_table_upserted requires is_active"));
    };
    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("dining_table_upserted requires a business"));
    };
    let branch_id = match operation.branch_id {
        Some(branch_id) => Some(branch_id.to_string()),
        None => default_branch_id(transaction, &business_id).await?,
    };
    let at = operation.created_at_ms;

    sqlx::query(
        "INSERT INTO dining_table (
            id, business_id, branch_id, name, zone, is_active,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT (id) DO UPDATE SET
            branch_id = excluded.branch_id,
            name = excluded.name,
            zone = excluded.zone,
            is_active = excluded.is_active,
            updated_at_ms = excluded.updated_at_ms",
    )
    .bind(operation.entity_id.to_string())
    .bind(business_id)
    .bind(branch_id)
    .bind(name)
    .bind(zone)
    .bind(is_active)
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn user_upserted(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(display_name) = text(payload, "display_name") else {
        return Ok(invalid("user_upserted requires a display name"));
    };
    let Some(role) = text(payload, "role") else {
        return Ok(invalid("user_upserted requires a role"));
    };
    if !matches!(role.as_str(), "admin" | "cashier" | "waiter" | "kitchen") {
        return Ok(invalid("invalid user role"));
    }
    let Some(is_active) = boolean(payload, "is_active") else {
        return Ok(invalid("user_upserted requires is_active"));
    };
    let pin_hash = optional_text(payload, "pin_hash");
    if pin_hash
        .as_ref()
        .is_some_and(|hash| !hash.starts_with("$argon2"))
    {
        return Ok(invalid("pin_hash must be an Argon2 verifier"));
    }
    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("user_upserted requires a business"));
    };
    let at = operation.created_at_ms;
    let id = operation.entity_id.to_string();

    let existing_pin: Option<String> =
        sqlx::query_scalar::<_, Option<String>>("SELECT pin_hash FROM app_user WHERE id = ?1")
            .bind(&id)
            .fetch_optional(&mut **transaction)
            .await?
            .flatten();

    let pin_hash = pin_hash.or(existing_pin);

    sqlx::query(
        "INSERT INTO app_user (
            id, business_id, display_name, role, pin_hash, is_active,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)
         ON CONFLICT (id) DO UPDATE SET
            display_name = excluded.display_name,
            role = excluded.role,
            pin_hash = excluded.pin_hash,
            is_active = excluded.is_active,
            updated_at_ms = excluded.updated_at_ms",
    )
    .bind(id)
    .bind(business_id)
    .bind(display_name)
    .bind(role)
    .bind(pin_hash)
    .bind(is_active)
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn business_id(
    transaction: &mut Transaction<'_, Sqlite>,
) -> Result<Option<String>, AppError> {
    Ok(
        sqlx::query_scalar("SELECT id FROM business ORDER BY created_at_ms LIMIT 1")
            .fetch_optional(&mut **transaction)
            .await?,
    )
}

async fn default_branch_id(
    transaction: &mut Transaction<'_, Sqlite>,
    business_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM branch
         WHERE business_id = ?1
         ORDER BY is_default DESC, created_at_ms
         LIMIT 1",
    )
    .bind(business_id)
    .fetch_optional(&mut **transaction)
    .await?)
}

fn invalid(reason: &str) -> ProjectionOutcome {
    ProjectionOutcome::Skipped(reason.to_owned())
}

fn text(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn optional_text(payload: &Value, field: &str) -> Option<String> {
    payload
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn integer(payload: &Value, field: &str) -> Option<i64> {
    payload.get(field).and_then(Value::as_i64)
}

fn boolean(payload: &Value, field: &str) -> Option<bool> {
    payload.get(field).and_then(Value::as_bool)
}

/// Reads one projected setting.
pub async fn setting(
    pool: &sqlx::SqlitePool,
    business_id: &str,
    key: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT value FROM business_setting
         WHERE business_id = ?1 AND key = ?2",
    )
    .bind(business_id)
    .bind(key)
    .fetch_optional(pool)
    .await?)
}

/// Lists projected menu categories and items for the web app.
pub async fn menu(
    pool: &sqlx::SqlitePool,
    business_id: &str,
) -> Result<Vec<ProjectedCategory>, AppError> {
    let category_rows = sqlx::query(
        "SELECT id, name, sort_order, is_active
         FROM menu_category
         WHERE business_id = ?1
         ORDER BY sort_order, name",
    )
    .bind(business_id)
    .fetch_all(pool)
    .await?;

    let item_rows = sqlx::query(
        "SELECT
            id, category_id, name, description, price_minor, currency,
            is_active, is_available, sort_order
         FROM menu_item
         WHERE business_id = ?1
         ORDER BY sort_order, name",
    )
    .bind(business_id)
    .fetch_all(pool)
    .await?;

    let mut categories = Vec::with_capacity(category_rows.len());

    for row in category_rows {
        let id: String = row.try_get("id")?;
        let mut items = Vec::new();

        for item in &item_rows {
            let category_id: Option<String> = item.try_get("category_id")?;
            if category_id.as_deref() != Some(id.as_str()) {
                continue;
            }

            items.push(ProjectedItem {
                id: item.try_get("id")?,
                name: item.try_get("name")?,
                description: item.try_get("description")?,
                price_minor: item.try_get("price_minor")?,
                currency: item.try_get("currency")?,
                is_active: item.try_get("is_active")?,
                is_available: item.try_get("is_available")?,
            });
        }

        categories.push(ProjectedCategory {
            id,
            name: row.try_get("name")?,
            is_active: row.try_get("is_active")?,
            items,
        });
    }

    let mut uncategorized = Vec::new();
    for item in &item_rows {
        let category_id: Option<String> = item.try_get("category_id")?;
        if category_id.is_some() {
            continue;
        }

        uncategorized.push(ProjectedItem {
            id: item.try_get("id")?,
            name: item.try_get("name")?,
            description: item.try_get("description")?,
            price_minor: item.try_get("price_minor")?,
            currency: item.try_get("currency")?,
            is_active: item.try_get("is_active")?,
            is_available: item.try_get("is_available")?,
        });
    }

    if !uncategorized.is_empty() {
        categories.push(ProjectedCategory {
            id: String::new(),
            name: "Sin categoría".to_owned(),
            is_active: true,
            items: uncategorized,
        });
    }

    Ok(categories)
}

#[derive(Debug, Clone)]
pub struct ProjectedCategory {
    pub id: String,
    pub name: String,
    pub is_active: bool,
    pub items: Vec<ProjectedItem>,
}

#[derive(Debug, Clone)]
pub struct ProjectedItem {
    pub id: String,
    pub name: String,
    pub description: Option<String>,
    pub price_minor: i64,
    pub currency: String,
    pub is_active: bool,
    pub is_available: bool,
}

#[cfg(test)]
mod tests {
    use nexo_core::{DeviceId, Operation, OperationKind, Uuid};
    use serde_json::json;

    use super::*;
    use crate::sync;
    use crate::test_support;

    fn operation(kind: OperationKind, entity_id: Uuid, payload: Value) -> Operation {
        Operation::new(
            DeviceId::new(),
            None,
            kind,
            entity_id,
            payload,
            1_700_000_000_000,
        )
    }

    async fn business_id(pool: &sqlx::SqlitePool) -> String {
        sqlx::query_scalar("SELECT id FROM business ORDER BY created_at_ms LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn master_data_operations_are_projected_in_order() {
        let state = test_support::state().await;
        let category_id = Uuid::now_v7();
        let item_id = Uuid::now_v7();

        sync::push(
            &state.pool,
            vec![
                operation(
                    OperationKind::MenuCategoryUpserted,
                    category_id,
                    json!({ "name": "Desayunos", "sort_order": 0, "is_active": true }),
                ),
                operation(
                    OperationKind::MenuItemUpserted,
                    item_id,
                    json!({
                        "category_id": category_id.to_string(),
                        "name": "Arepa",
                        "description": "Con queso",
                        "price_minor": 250,
                        "currency": "USD",
                        "is_active": true,
                        "is_available": true,
                        "sort_order": 0
                    }),
                ),
            ],
        )
        .await
        .unwrap();

        let menu = menu(&state.pool, &business_id(&state.pool).await)
            .await
            .unwrap();

        assert_eq!(menu.len(), 1);
        assert_eq!(menu[0].name, "Desayunos");
        assert_eq!(menu[0].items.len(), 1);
        assert_eq!(menu[0].items[0].name, "Arepa");
        assert_eq!(menu[0].items[0].price_minor, 250);
        assert!(menu[0].items[0].is_available);
    }

    #[tokio::test]
    async fn availability_changes_are_narrow_updates() {
        let state = test_support::state().await;
        let item_id = Uuid::now_v7();

        sync::push(
            &state.pool,
            vec![operation(
                OperationKind::MenuItemUpserted,
                item_id,
                json!({
                    "name": "Café",
                    "price_minor": 100,
                    "currency": "USD",
                    "is_active": true,
                    "is_available": true,
                    "sort_order": 0
                }),
            )],
        )
        .await
        .unwrap();

        sync::push(
            &state.pool,
            vec![operation(
                OperationKind::MenuItemAvailabilityChanged,
                item_id,
                json!({ "is_available": false }),
            )],
        )
        .await
        .unwrap();

        let menu = menu(&state.pool, &business_id(&state.pool).await)
            .await
            .unwrap();

        assert!(!menu[0].items[0].is_available);
    }

    #[tokio::test]
    async fn invalid_payloads_are_stored_as_anomalies_without_projecting() {
        let state = test_support::state().await;

        sync::push(
            &state.pool,
            vec![operation(
                OperationKind::MenuItemUpserted,
                Uuid::now_v7(),
                json!({
                    "name": "Bad",
                    "price_minor": -1,
                    "currency": "USD",
                    "is_active": true,
                    "is_available": true,
                    "sort_order": 0
                }),
            )],
        )
        .await
        .unwrap();

        let items: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM menu_item")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let anomalies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_anomaly")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        assert_eq!(items, 0);
        assert_eq!(anomalies, 1);
    }
}
