//! Server projections of the operation log.
//!
//! Projections are a deterministic fold in server sequence order. Phase 2
//! covers master data; order, sale, and cash projections arrive with their
//! phases. An operation whose payload is invalid is stored in the log and
//! recorded as an anomaly, but it never changes the projection.

use nexo_core::{Operation, OperationKind, Uuid};
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

/// Applies one operation to the projection.
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
        OperationKind::OrderOpened => order_opened(transaction, operation).await,
        OperationKind::OrderLineAdded => order_line_added(transaction, operation).await,
        OperationKind::OrderSent => order_sent(transaction, operation).await,
        OperationKind::OrderLineVoided => order_line_voided(transaction, operation).await,
        OperationKind::OrderLineReady => order_line_ready(transaction, operation).await,
        OperationKind::OrderLineServed => order_line_served(transaction, operation).await,
        OperationKind::OrderCancelled => order_cancelled(transaction, operation).await,
        OperationKind::OrderBilled => order_billed(transaction, operation).await,
        OperationKind::SaleCreated => sale_created(transaction, operation).await,
        OperationKind::SaleCancelled => sale_cancelled(transaction, operation).await,
        OperationKind::PaymentRecorded => payment_recorded(transaction, operation).await,
        OperationKind::CashSessionOpened => cash_session_opened(transaction, operation).await,
        OperationKind::CashSessionClosed => cash_session_closed(transaction, operation).await,
        OperationKind::CashMovementRecorded => cash_movement_recorded(transaction, operation).await,
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

async fn order_opened(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(order_type) = text(payload, "order_type") else {
        return Ok(invalid("order_opened requires an order type"));
    };
    if order_type != "dine_in" && order_type != "takeout" {
        return Ok(invalid("invalid order type"));
    }
    let Some(display_code) = text(payload, "display_code") else {
        return Ok(invalid("order_opened requires a display code"));
    };
    let table_id = optional_text(payload, "table_id");

    if order_type == "dine_in" && table_id.is_none() {
        return Ok(invalid("a dine-in order requires a table"));
    }
    if order_type == "takeout" && table_id.is_some() {
        return Ok(invalid("a takeout order cannot reference a table"));
    }

    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("order_opened requires a business"));
    };
    let branch_id = match operation.branch_id {
        Some(branch_id) => branch_id.to_string(),
        None => match default_branch_id(transaction, &business_id).await? {
            Some(branch_id) => branch_id,
            None => return Ok(invalid("the branch does not exist")),
        },
    };

    let branch_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM branch WHERE id = ?1 AND business_id = ?2
         )",
    )
    .bind(&branch_id)
    .bind(&business_id)
    .fetch_one(&mut **transaction)
    .await?;
    if !branch_exists {
        return Ok(invalid("the branch does not exist"));
    }

    let device_id = operation.device_id.to_string();
    let device_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM device WHERE id = ?1)")
            .bind(&device_id)
            .fetch_one(&mut **transaction)
            .await?;
    if !device_exists {
        return Ok(invalid("the device does not exist"));
    }

    if let Some(table_id) = &table_id {
        let table_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM dining_table
                WHERE id = ?1 AND business_id = ?2 AND branch_id = ?3
             )",
        )
        .bind(table_id)
        .bind(&business_id)
        .bind(&branch_id)
        .fetch_one(&mut **transaction)
        .await?;
        if !table_exists {
            return Ok(invalid("the dining table does not exist"));
        }

        let occupied: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM restaurant_order
                WHERE table_id = ?1 AND status IN ('open', 'active', 'served')
             )",
        )
        .bind(table_id)
        .fetch_one(&mut **transaction)
        .await?;
        if occupied {
            return Ok(invalid("the dining table already has an open order"));
        }
    }

    let opened_by = match optional_text(payload, "opened_by") {
        Some(user_id) => {
            if !user_exists(transaction, &user_id).await? {
                return Ok(invalid("the user does not exist"));
            }
            Some(user_id)
        }
        None => None,
    };

    let opened_at_ms = integer(payload, "opened_at_ms").unwrap_or(operation.created_at_ms);
    let order_id = operation.entity_id.to_string();

    sqlx::query(
        "INSERT INTO restaurant_order (
            id, business_id, branch_id, device_id, order_type, table_id,
            customer_name, display_code, status, notes, opened_by,
            opened_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9, ?10, ?11)",
    )
    .bind(&order_id)
    .bind(&business_id)
    .bind(&branch_id)
    .bind(&device_id)
    .bind(&order_type)
    .bind(table_id.as_deref())
    .bind(optional_text(payload, "customer_name"))
    .bind(&display_code)
    .bind(optional_text(payload, "notes"))
    .bind(opened_by.as_deref())
    .bind(opened_at_ms)
    .execute(&mut **transaction)
    .await?;

    record_order_event(
        transaction,
        &order_id,
        "order_opened",
        opened_by.as_deref(),
        operation,
        payload.clone(),
        opened_at_ms,
    )
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn order_line_added(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let Some(order_id) = text(payload, "order_id") else {
        return Ok(invalid("order_line_added requires an order"));
    };
    let Some(description) = text(payload, "description") else {
        return Ok(invalid("order_line_added requires a description"));
    };
    let Some(quantity) = integer(payload, "quantity").filter(|quantity| *quantity > 0) else {
        return Ok(invalid("quantity must be greater than zero"));
    };
    let Some(unit_price_minor) = integer(payload, "unit_price_minor").filter(|price| *price >= 0)
    else {
        return Ok(invalid("unit_price_minor cannot be negative"));
    };
    let Some(line_total_minor) = integer(payload, "line_total_minor") else {
        return Ok(invalid("order_line_added requires a line total"));
    };
    if unit_price_minor.checked_mul(quantity) != Some(line_total_minor) {
        return Ok(invalid(
            "the line total does not match price times quantity",
        ));
    }
    let Some(currency) = text(payload, "currency") else {
        return Ok(invalid("order_line_added requires a currency"));
    };
    if currency.len() != 3 {
        return Ok(invalid("currency must have three letters"));
    }
    let Some(round_number) = integer(payload, "round_number").filter(|round| *round > 0) else {
        return Ok(invalid("round_number must be greater than zero"));
    };

    let order_status: Option<String> =
        sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
            .bind(&order_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some(order_status) = order_status else {
        return Ok(invalid("the order does not exist"));
    };
    if !matches!(order_status.as_str(), "open" | "active" | "served") {
        return Ok(invalid("the order cannot receive new lines"));
    }

    let menu_item_id = optional_text(payload, "menu_item_id");
    let menu_item_id = match menu_item_id {
        Some(item_id) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM menu_item WHERE id = ?1)")
                    .bind(&item_id)
                    .fetch_one(&mut **transaction)
                    .await?;
            exists.then_some(item_id)
        }
        None => None,
    };

    let created_by = match optional_text(payload, "created_by") {
        Some(user_id) => {
            if !user_exists(transaction, &user_id).await? {
                return Ok(invalid("the user does not exist"));
            }
            Some(user_id)
        }
        None => None,
    };

    let created_at_ms = integer(payload, "created_at_ms").unwrap_or(operation.created_at_ms);
    let line_id = operation.entity_id.to_string();

    sqlx::query(
        "INSERT INTO order_line (
            id, order_id, menu_item_id, description, quantity,
            unit_price_minor, line_total_minor, currency, notes, status,
            round_number, created_at_ms, created_by
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'pending', ?10, ?11, ?12)",
    )
    .bind(&line_id)
    .bind(&order_id)
    .bind(menu_item_id.as_deref())
    .bind(&description)
    .bind(quantity)
    .bind(unit_price_minor)
    .bind(line_total_minor)
    .bind(&currency)
    .bind(optional_text(payload, "notes"))
    .bind(round_number)
    .bind(created_at_ms)
    .bind(created_by.as_deref())
    .execute(&mut **transaction)
    .await?;

    record_order_event(
        transaction,
        &order_id,
        "order_line_added",
        created_by.as_deref(),
        operation,
        payload.clone(),
        created_at_ms,
    )
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn order_sent(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let order_id = operation.entity_id.to_string();

    let Some(round_number) = integer(payload, "round_number").filter(|round| *round > 0) else {
        return Ok(invalid("order_sent requires a round number"));
    };
    let Some(line_ids) = string_array(payload, "line_ids") else {
        return Ok(invalid("order_sent requires the line ids"));
    };
    if line_ids.is_empty() {
        return Ok(invalid("order_sent requires at least one line"));
    }

    let order_status: Option<String> =
        sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
            .bind(&order_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some(order_status) = order_status else {
        return Ok(invalid("the order does not exist"));
    };
    if !matches!(order_status.as_str(), "open" | "active" | "served") {
        return Ok(invalid("the order cannot be sent to the kitchen"));
    }

    for line_id in &line_ids {
        let row = sqlx::query(
            "SELECT order_id, status, round_number
             FROM order_line WHERE id = ?1",
        )
        .bind(line_id)
        .fetch_optional(&mut **transaction)
        .await?;
        let Some(row) = row else {
            return Ok(invalid("an order line does not exist"));
        };
        let line_order_id: String = row.try_get("order_id")?;
        let status: String = row.try_get("status")?;
        let round: i64 = row.try_get("round_number")?;
        if line_order_id != order_id || status != "pending" || round != round_number {
            return Ok(invalid("the line does not belong to the pending round"));
        }
    }

    let sent_at_ms = integer(payload, "sent_at_ms").unwrap_or(operation.created_at_ms);

    for line_id in &line_ids {
        sqlx::query(
            "UPDATE order_line
             SET status = 'sent', sent_at_ms = ?2
             WHERE id = ?1 AND status = 'pending'",
        )
        .bind(line_id)
        .bind(sent_at_ms)
        .execute(&mut **transaction)
        .await?;
    }

    sqlx::query(
        "UPDATE restaurant_order
         SET status = 'active', closed_at_ms = NULL
         WHERE id = ?1",
    )
    .bind(&order_id)
    .execute(&mut **transaction)
    .await?;

    record_order_event(
        transaction,
        &order_id,
        "order_sent",
        optional_text(payload, "sent_by").as_deref(),
        operation,
        payload.clone(),
        sent_at_ms,
    )
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn order_line_voided(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let line_id = operation.entity_id.to_string();

    let Some(order_id) = text(payload, "order_id") else {
        return Ok(invalid("order_line_voided requires an order"));
    };
    let Some(reason) = text(payload, "reason") else {
        return Ok(invalid("order_line_voided requires a reason"));
    };

    let row = sqlx::query("SELECT order_id, status FROM order_line WHERE id = ?1")
        .bind(&line_id)
        .fetch_optional(&mut **transaction)
        .await?;
    let Some(row) = row else {
        return Ok(invalid("the order line does not exist"));
    };
    let line_order_id: String = row.try_get("order_id")?;
    let status: String = row.try_get("status")?;
    if line_order_id != order_id || !matches!(status.as_str(), "pending" | "sent" | "ready") {
        return Ok(invalid("the line cannot be voided"));
    }

    let voided_at_ms = integer(payload, "voided_at_ms").unwrap_or(operation.created_at_ms);

    sqlx::query(
        "UPDATE order_line
         SET status = 'voided', voided_at_ms = ?2, void_reason = ?3
         WHERE id = ?1",
    )
    .bind(&line_id)
    .bind(voided_at_ms)
    .bind(&reason)
    .execute(&mut **transaction)
    .await?;

    record_order_event(
        transaction,
        &order_id,
        "order_line_voided",
        optional_text(payload, "voided_by").as_deref(),
        operation,
        payload.clone(),
        voided_at_ms,
    )
    .await?;

    refresh_order_status(transaction, &order_id).await?;

    Ok(ProjectionOutcome::Applied)
}

async fn order_line_ready(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let line_id = operation.entity_id.to_string();

    let Some(order_id) = text(payload, "order_id") else {
        return Ok(invalid("order_line_ready requires an order"));
    };

    let row = sqlx::query("SELECT order_id, status FROM order_line WHERE id = ?1")
        .bind(&line_id)
        .fetch_optional(&mut **transaction)
        .await?;
    let Some(row) = row else {
        return Ok(invalid("the order line does not exist"));
    };
    let line_order_id: String = row.try_get("order_id")?;
    let status: String = row.try_get("status")?;
    if line_order_id != order_id || status != "sent" {
        return Ok(invalid("the line cannot be marked ready"));
    }

    let ready_at_ms = integer(payload, "ready_at_ms").unwrap_or(operation.created_at_ms);

    sqlx::query("UPDATE order_line SET status = 'ready', ready_at_ms = ?2 WHERE id = ?1")
        .bind(&line_id)
        .bind(ready_at_ms)
        .execute(&mut **transaction)
        .await?;

    record_order_event(
        transaction,
        &order_id,
        "order_line_ready",
        None,
        operation,
        payload.clone(),
        ready_at_ms,
    )
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn order_line_served(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let line_id = operation.entity_id.to_string();

    let Some(order_id) = text(payload, "order_id") else {
        return Ok(invalid("order_line_served requires an order"));
    };

    let row = sqlx::query("SELECT order_id, status FROM order_line WHERE id = ?1")
        .bind(&line_id)
        .fetch_optional(&mut **transaction)
        .await?;
    let Some(row) = row else {
        return Ok(invalid("the order line does not exist"));
    };
    let line_order_id: String = row.try_get("order_id")?;
    let status: String = row.try_get("status")?;
    if line_order_id != order_id || !matches!(status.as_str(), "sent" | "ready") {
        return Ok(invalid("the line cannot be marked served"));
    }

    let served_at_ms = integer(payload, "served_at_ms").unwrap_or(operation.created_at_ms);

    sqlx::query("UPDATE order_line SET status = 'served', served_at_ms = ?2 WHERE id = ?1")
        .bind(&line_id)
        .bind(served_at_ms)
        .execute(&mut **transaction)
        .await?;

    refresh_order_status(transaction, &order_id).await?;

    record_order_event(
        transaction,
        &order_id,
        "order_line_served",
        optional_text(payload, "served_by").as_deref(),
        operation,
        payload.clone(),
        served_at_ms,
    )
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn order_cancelled(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let order_id = operation.entity_id.to_string();

    let Some(reason) = text(payload, "reason") else {
        return Ok(invalid("order_cancelled requires a reason"));
    };

    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
            .bind(&order_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some(status) = status else {
        return Ok(invalid("the order does not exist"));
    };
    if !matches!(status.as_str(), "open" | "active" | "served") {
        return Ok(invalid("the order cannot be cancelled"));
    }

    let cancelled_at_ms = integer(payload, "cancelled_at_ms").unwrap_or(operation.created_at_ms);

    sqlx::query(
        "UPDATE restaurant_order
         SET status = 'cancelled', closed_at_ms = ?2, cancel_reason = ?3
         WHERE id = ?1",
    )
    .bind(&order_id)
    .bind(cancelled_at_ms)
    .bind(&reason)
    .execute(&mut **transaction)
    .await?;

    record_order_event(
        transaction,
        &order_id,
        "order_cancelled",
        optional_text(payload, "cancelled_by").as_deref(),
        operation,
        payload.clone(),
        cancelled_at_ms,
    )
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn refresh_order_status(
    transaction: &mut Transaction<'_, Sqlite>,
    order_id: &str,
) -> Result<(), AppError> {
    let row = sqlx::query(
        "SELECT
            o.status,
            COUNT(*) FILTER (WHERE l.status <> 'voided') AS total,
            COUNT(*) FILTER (WHERE l.status = 'served') AS served
         FROM restaurant_order o
         LEFT JOIN order_line l ON l.order_id = o.id
         WHERE o.id = ?1
         GROUP BY o.id",
    )
    .bind(order_id)
    .fetch_optional(&mut **transaction)
    .await?;
    let Some(row) = row else {
        return Ok(());
    };
    let status: String = row.try_get("status")?;
    let total: i64 = row.try_get("total")?;
    let served: i64 = row.try_get("served")?;

    if status == "active" && total > 0 && served == total {
        sqlx::query("UPDATE restaurant_order SET status = 'served' WHERE id = ?1")
            .bind(order_id)
            .execute(&mut **transaction)
            .await?;
    }

    Ok(())
}

async fn order_billed(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let order_id = operation.entity_id.to_string();

    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
            .bind(&order_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some(status) = status else {
        return Ok(invalid("the order does not exist"));
    };
    if status == "billed" {
        return Ok(ProjectionOutcome::Applied);
    }
    if !matches!(status.as_str(), "open" | "active" | "served") {
        return Ok(invalid("the order cannot be billed"));
    }

    // The billing pair is commutative: `sale_created` and `order_billed` can
    // arrive in either order because both are written in the same desktop
    // transaction. This projection only changes the order; the sale is
    // projected independently.
    let billed_at_ms = integer(payload, "created_at_ms").unwrap_or(operation.created_at_ms);
    let created_by = optional_user(transaction, payload, "created_by").await?;

    sqlx::query(
        "UPDATE restaurant_order
         SET status = 'billed', closed_at_ms = ?2
         WHERE id = ?1 AND status IN ('open', 'active', 'served')",
    )
    .bind(&order_id)
    .bind(billed_at_ms)
    .execute(&mut **transaction)
    .await?;

    record_order_event(
        transaction,
        &order_id,
        "order_billed",
        created_by.as_deref(),
        operation,
        payload.clone(),
        billed_at_ms,
    )
    .await?;

    Ok(ProjectionOutcome::Applied)
}

struct SaleLineInput {
    id: String,
    order_line_id: Option<String>,
    menu_item_id: Option<String>,
    description: String,
    quantity: i64,
    unit_price_minor: i64,
    line_total_minor: i64,
    currency: String,
}

struct PaymentInput {
    id: String,
    method_id: Option<String>,
    amount_minor: i64,
    currency: String,
    base_amount_minor: i64,
    exchange_rate_micros: Option<i64>,
    reference: Option<String>,
    tendered_minor: Option<i64>,
    change_minor: Option<i64>,
    created_at_ms: i64,
    created_by: Option<String>,
    device_id: String,
}

async fn sale_created(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let sale_id = operation.entity_id.to_string();

    let already_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sale WHERE id = ?1)")
            .bind(&sale_id)
            .fetch_one(&mut **transaction)
            .await?;
    if already_exists {
        return Ok(ProjectionOutcome::Applied);
    }

    let Some(order_id) = text(payload, "order_id") else {
        return Ok(invalid("sale_created requires an order"));
    };
    let order: Option<(String, String)> =
        sqlx::query_as("SELECT branch_id, status FROM restaurant_order WHERE id = ?1")
            .bind(&order_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some((branch_id, order_status)) = order else {
        return Ok(invalid("the order does not exist"));
    };
    if operation
        .branch_id
        .is_some_and(|operation_branch| operation_branch.to_string() != branch_id)
    {
        return Ok(invalid("the order belongs to another branch"));
    }
    if order_status == "cancelled" {
        return Ok(invalid("a cancelled order cannot be billed"));
    }

    let conflicting_sale: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM sale WHERE order_id = ?1)")
            .bind(&order_id)
            .fetch_one(&mut **transaction)
            .await?;
    if conflicting_sale {
        return Ok(invalid("the order already has a sale"));
    }

    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("sale_created requires a business"));
    };
    let device_id = operation.device_id.to_string();
    if !device_exists(transaction, &device_id).await? {
        return Ok(invalid("the device does not exist"));
    }

    let Some(number) = text(payload, "number") else {
        return Ok(invalid("sale_created requires a sale number"));
    };
    let Some(subtotal_minor) = integer(payload, "subtotal_minor").filter(|value| *value >= 0)
    else {
        return Ok(invalid("subtotal_minor cannot be negative"));
    };
    let Some(tax_minor) = integer(payload, "tax_minor").filter(|value| *value >= 0) else {
        return Ok(invalid("tax_minor cannot be negative"));
    };
    let Some(total_minor) = integer(payload, "total_minor").filter(|value| *value > 0) else {
        return Ok(invalid("total_minor must be greater than zero"));
    };
    if subtotal_minor + tax_minor != total_minor {
        return Ok(invalid("subtotal and IVA must add up to the total"));
    }
    let Some(currency) = text(payload, "currency") else {
        return Ok(invalid("sale_created requires a currency"));
    };
    if currency.len() != 3 {
        return Ok(invalid("currency must have three letters"));
    }
    let tax_rate_basis_points = integer(payload, "tax_rate_basis_points");
    if tax_rate_basis_points.is_some_and(|rate| !(0..=10_000).contains(&rate)) {
        return Ok(invalid("the IVA rate is out of range"));
    }
    let cash_session_id = optional_text(payload, "cash_session_id");
    if let Some(session_id) = &cash_session_id {
        let session_exists: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM cash_session
                WHERE id = ?1 AND business_id = ?2 AND branch_id = ?3
             )",
        )
        .bind(session_id)
        .bind(&business_id)
        .bind(&branch_id)
        .fetch_one(&mut **transaction)
        .await?;
        if !session_exists {
            return Ok(invalid("the cash session does not exist"));
        }
    }

    let created_by = optional_user(transaction, payload, "created_by").await?;
    let created_at_ms = integer(payload, "created_at_ms").unwrap_or(operation.created_at_ms);

    let Some(lines) = payload.get("lines").and_then(Value::as_array) else {
        return Ok(invalid("sale_created requires its lines"));
    };
    let mut sale_lines = Vec::with_capacity(lines.len());
    let mut line_total_sum: i64 = 0;

    for line in lines {
        let Some(line_id) = line.get("line_id").and_then(Value::as_str) else {
            return Ok(invalid("each sale line requires an id"));
        };
        if Uuid::parse_str(line_id).is_err() {
            return Ok(invalid("invalid sale line id"));
        }
        let Some(description) = line
            .get("description")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return Ok(invalid("each sale line requires a description"));
        };
        let Some(quantity) = line
            .get("quantity")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
        else {
            return Ok(invalid("sale line quantities must be greater than zero"));
        };
        let Some(unit_price_minor) = line
            .get("unit_price_minor")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
        else {
            return Ok(invalid("sale line prices cannot be negative"));
        };
        let Some(line_total_minor) = line
            .get("line_total_minor")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0)
        else {
            return Ok(invalid("sale line totals cannot be negative"));
        };
        if unit_price_minor.checked_mul(quantity) != Some(line_total_minor) {
            return Ok(invalid(
                "the sale line total does not match price times quantity",
            ));
        }
        let Some(line_currency) = line.get("currency").and_then(Value::as_str) else {
            return Ok(invalid("each sale line requires a currency"));
        };
        if line_currency.len() != 3 {
            return Ok(invalid("currency must have three letters"));
        }

        let order_line_id = match line.get("order_line_id").and_then(Value::as_str) {
            Some(line_id) => {
                let belongs: bool = sqlx::query_scalar(
                    "SELECT EXISTS (
                        SELECT 1 FROM order_line WHERE id = ?1 AND order_id = ?2
                     )",
                )
                .bind(line_id)
                .bind(&order_id)
                .fetch_one(&mut **transaction)
                .await?;
                belongs.then(|| line_id.to_owned())
            }
            None => None,
        };
        let menu_item_id = match line.get("menu_item_id").and_then(Value::as_str) {
            Some(item_id) => {
                let exists: bool =
                    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM menu_item WHERE id = ?1)")
                        .bind(item_id)
                        .fetch_one(&mut **transaction)
                        .await?;
                exists.then(|| item_id.to_owned())
            }
            None => None,
        };

        line_total_sum += line_total_minor;
        sale_lines.push(SaleLineInput {
            id: line_id.to_owned(),
            order_line_id,
            menu_item_id,
            description: description.to_owned(),
            quantity,
            unit_price_minor,
            line_total_minor,
            currency: line_currency.to_owned(),
        });
    }

    if line_total_sum != total_minor {
        return Ok(invalid("the sale lines do not add up to the total"));
    }

    let Some(payments) = payload.get("payments").and_then(Value::as_array) else {
        return Ok(invalid("sale_created requires its payments"));
    };
    let mut sale_payments = Vec::with_capacity(payments.len());
    let mut payment_base_sum: i64 = 0;

    for payment in payments {
        let Some(payment_id) = payment.get("payment_id").and_then(Value::as_str) else {
            return Ok(invalid("each payment requires an id"));
        };
        if Uuid::parse_str(payment_id).is_err() {
            return Ok(invalid("invalid payment id"));
        }
        let Some(amount_minor) = payment
            .get("amount_minor")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
        else {
            return Ok(invalid("payment amounts must be greater than zero"));
        };
        let Some(payment_currency) = payment.get("currency").and_then(Value::as_str) else {
            return Ok(invalid("each payment requires a currency"));
        };
        if payment_currency.len() != 3 {
            return Ok(invalid("currency must have three letters"));
        }
        let Some(base_amount_minor) = payment
            .get("base_amount_minor")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0)
        else {
            return Ok(invalid("payment base amounts must be greater than zero"));
        };
        let exchange_rate_micros = payment.get("exchange_rate_micros").and_then(Value::as_i64);
        if exchange_rate_micros.is_some_and(|rate| rate <= 0) {
            return Ok(invalid("the exchange rate must be greater than zero"));
        }
        if payment_currency != currency && exchange_rate_micros.is_none() {
            return Ok(invalid(
                "secondary-currency payments require an exchange rate",
            ));
        }

        let method_id = match payment.get("method_id").and_then(Value::as_str) {
            Some(method_id) => {
                let exists: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM payment_method WHERE id = ?1)",
                )
                .bind(method_id)
                .fetch_one(&mut **transaction)
                .await?;
                exists.then(|| method_id.to_owned())
            }
            None => None,
        };
        let tendered_minor = payment
            .get("tendered_minor")
            .and_then(Value::as_i64)
            .filter(|value| *value > 0);
        let change_minor = payment
            .get("change_minor")
            .and_then(Value::as_i64)
            .filter(|value| *value >= 0);
        if let Some(tendered) = tendered_minor {
            if tendered < amount_minor {
                return Ok(invalid(
                    "the tendered amount cannot be less than the payment",
                ));
            }
            if change_minor.is_some_and(|change| change != tendered - amount_minor) {
                return Ok(invalid("the change does not match the tendered amount"));
            }
        } else if change_minor.is_some() {
            return Ok(invalid("change requires a tendered amount"));
        }

        let payment_device_id = resolve_device(transaction, payment, operation.device_id).await?;
        let payment_created_by = optional_user(transaction, payment, "created_by").await?;
        let payment_created_at_ms = payment
            .get("created_at_ms")
            .and_then(Value::as_i64)
            .unwrap_or(created_at_ms);

        payment_base_sum += base_amount_minor;
        sale_payments.push(PaymentInput {
            id: payment_id.to_owned(),
            method_id,
            amount_minor,
            currency: payment_currency.to_owned(),
            base_amount_minor,
            exchange_rate_micros,
            reference: payment
                .get("reference")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            tendered_minor,
            change_minor,
            created_at_ms: payment_created_at_ms,
            created_by: payment_created_by,
            device_id: payment_device_id,
        });
    }

    if payment_base_sum != total_minor {
        return Ok(invalid("the payments do not add up to the sale total"));
    }

    sqlx::query(
        "INSERT INTO sale (
            id, business_id, branch_id, device_id, order_id, cash_session_id,
            number, status, subtotal_minor, tax_minor, total_minor, currency,
            tax_name, tax_rate_basis_points, created_at_ms, created_by
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, 'completed', ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, ?15
         )",
    )
    .bind(&sale_id)
    .bind(&business_id)
    .bind(&branch_id)
    .bind(&device_id)
    .bind(&order_id)
    .bind(cash_session_id.as_deref())
    .bind(&number)
    .bind(subtotal_minor)
    .bind(tax_minor)
    .bind(total_minor)
    .bind(&currency)
    .bind(optional_text(payload, "tax_name"))
    .bind(tax_rate_basis_points)
    .bind(created_at_ms)
    .bind(created_by.as_deref())
    .execute(&mut **transaction)
    .await?;

    for line in &sale_lines {
        sqlx::query(
            "INSERT INTO sale_line (
                id, sale_id, order_line_id, menu_item_id, description,
                quantity, unit_price_minor, line_total_minor, currency,
                created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        )
        .bind(&line.id)
        .bind(&sale_id)
        .bind(line.order_line_id.as_deref())
        .bind(line.menu_item_id.as_deref())
        .bind(&line.description)
        .bind(line.quantity)
        .bind(line.unit_price_minor)
        .bind(line.line_total_minor)
        .bind(&line.currency)
        .bind(created_at_ms)
        .execute(&mut **transaction)
        .await?;
    }

    for payment in &sale_payments {
        sqlx::query(
            "INSERT INTO payment (
                id, business_id, sale_id, method_id, amount_minor, currency,
                base_amount_minor, exchange_rate_micros, reference,
                tendered_minor, change_minor, created_at_ms, created_by,
                device_id
             ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
             )",
        )
        .bind(&payment.id)
        .bind(&business_id)
        .bind(&sale_id)
        .bind(payment.method_id.as_deref())
        .bind(payment.amount_minor)
        .bind(&payment.currency)
        .bind(payment.base_amount_minor)
        .bind(payment.exchange_rate_micros)
        .bind(payment.reference.as_deref())
        .bind(payment.tendered_minor)
        .bind(payment.change_minor)
        .bind(payment.created_at_ms)
        .bind(payment.created_by.as_deref())
        .bind(&payment.device_id)
        .execute(&mut **transaction)
        .await?;
    }

    Ok(ProjectionOutcome::Applied)
}

async fn sale_cancelled(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let sale_id = operation.entity_id.to_string();

    let sale: Option<(String, String, String)> =
        sqlx::query_as("SELECT business_id, branch_id, status FROM sale WHERE id = ?1")
            .bind(&sale_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some((business_id, branch_id, status)) = sale else {
        return Ok(invalid("the sale does not exist"));
    };
    if status == "cancelled" {
        return Ok(ProjectionOutcome::Applied);
    }

    let Some(reason) = text(payload, "reason") else {
        return Ok(invalid("sale_cancelled requires a reason"));
    };
    let cancelled_at_ms = integer(payload, "cancelled_at_ms").unwrap_or(operation.created_at_ms);
    let cancelled_by = optional_user(transaction, payload, "cancelled_by").await?;

    sqlx::query(
        "UPDATE sale
         SET status = 'cancelled', cancelled_at_ms = ?2, cancel_reason = ?3
         WHERE id = ?1 AND status = 'completed'",
    )
    .bind(&sale_id)
    .bind(cancelled_at_ms)
    .bind(&reason)
    .execute(&mut **transaction)
    .await?;

    sqlx::query(
        "INSERT INTO audit_event (
            id, business_id, branch_id, actor_user_id, device_id, action,
            entity_type, entity_id, details, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, 'sale_cancelled', 'sale', ?6, ?7, ?8)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(&business_id)
    .bind(&branch_id)
    .bind(cancelled_by.as_deref())
    .bind(operation.device_id.to_string())
    .bind(&sale_id)
    .bind(payload.to_string())
    .bind(cancelled_at_ms)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn payment_recorded(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let payment_id = operation.entity_id.to_string();

    let already_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM payment WHERE id = ?1)")
            .bind(&payment_id)
            .fetch_one(&mut **transaction)
            .await?;
    if already_exists {
        return Ok(ProjectionOutcome::Applied);
    }

    let Some(sale_id) = text(payload, "sale_id") else {
        return Ok(invalid("payment_recorded requires a sale"));
    };
    let sale: Option<(String, String)> =
        sqlx::query_as("SELECT business_id, currency FROM sale WHERE id = ?1")
            .bind(&sale_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some((business_id, sale_currency)) = sale else {
        return Ok(invalid("the sale does not exist"));
    };
    let Some(amount_minor) = integer(payload, "amount_minor").filter(|value| *value > 0) else {
        return Ok(invalid("payment amounts must be greater than zero"));
    };
    let Some(currency) = text(payload, "currency") else {
        return Ok(invalid("payment_recorded requires a currency"));
    };
    if currency.len() != 3 {
        return Ok(invalid("currency must have three letters"));
    }
    let Some(base_amount_minor) = integer(payload, "base_amount_minor").filter(|value| *value > 0)
    else {
        return Ok(invalid("payment base amounts must be greater than zero"));
    };
    let exchange_rate_micros = integer(payload, "exchange_rate_micros");
    if exchange_rate_micros.is_some_and(|rate| rate <= 0) {
        return Ok(invalid("the exchange rate must be greater than zero"));
    }
    if currency != sale_currency && exchange_rate_micros.is_none() {
        return Ok(invalid(
            "secondary-currency payments require an exchange rate",
        ));
    }

    let method_id = match optional_text(payload, "method_id") {
        Some(method_id) => {
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM payment_method WHERE id = ?1)")
                    .bind(&method_id)
                    .fetch_one(&mut **transaction)
                    .await?;
            exists.then_some(method_id)
        }
        None => None,
    };
    let tendered_minor = integer(payload, "tendered_minor").filter(|value| *value > 0);
    let change_minor = integer(payload, "change_minor").filter(|value| *value >= 0);
    if let Some(tendered) = tendered_minor {
        if tendered < amount_minor {
            return Ok(invalid(
                "the tendered amount cannot be less than the payment",
            ));
        }
        if change_minor.is_some_and(|change| change != tendered - amount_minor) {
            return Ok(invalid("the change does not match the tendered amount"));
        }
    } else if change_minor.is_some() {
        return Ok(invalid("change requires a tendered amount"));
    }

    let device_id = resolve_device(transaction, payload, operation.device_id).await?;
    let created_by = optional_user(transaction, payload, "created_by").await?;
    let created_at_ms = integer(payload, "created_at_ms").unwrap_or(operation.created_at_ms);

    sqlx::query(
        "INSERT INTO payment (
            id, business_id, sale_id, method_id, amount_minor, currency,
            base_amount_minor, exchange_rate_micros, reference,
            tendered_minor, change_minor, created_at_ms, created_by,
            device_id
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14
         )",
    )
    .bind(&payment_id)
    .bind(&business_id)
    .bind(&sale_id)
    .bind(method_id.as_deref())
    .bind(amount_minor)
    .bind(&currency)
    .bind(base_amount_minor)
    .bind(exchange_rate_micros)
    .bind(optional_text(payload, "reference"))
    .bind(tendered_minor)
    .bind(change_minor)
    .bind(created_at_ms)
    .bind(created_by.as_deref())
    .bind(&device_id)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn cash_session_opened(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let session_id = operation.entity_id.to_string();

    let already_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cash_session WHERE id = ?1)")
            .bind(&session_id)
            .fetch_one(&mut **transaction)
            .await?;
    if already_exists {
        return Ok(ProjectionOutcome::Applied);
    }

    let Some(business_id) = business_id(transaction).await? else {
        return Ok(invalid("cash_session_opened requires a business"));
    };
    let branch_id = match operation.branch_id {
        Some(branch_id) => branch_id.to_string(),
        None => match default_branch_id(transaction, &business_id).await? {
            Some(branch_id) => branch_id,
            None => return Ok(invalid("the branch does not exist")),
        },
    };
    let branch_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM branch WHERE id = ?1 AND business_id = ?2
         )",
    )
    .bind(&branch_id)
    .bind(&business_id)
    .fetch_one(&mut **transaction)
    .await?;
    if !branch_exists {
        return Ok(invalid("the branch does not exist"));
    }

    let device_id = resolve_device(transaction, payload, operation.device_id).await?;
    if !device_exists(transaction, &device_id).await? {
        return Ok(invalid("the device does not exist"));
    }

    let Some(opening_amount_minor) =
        integer(payload, "opening_amount_minor").filter(|value| *value >= 0)
    else {
        return Ok(invalid("the opening amount cannot be negative"));
    };
    let Some(secondary_opening_amount_minor) =
        integer(payload, "secondary_opening_amount_minor").filter(|value| *value >= 0)
    else {
        return Ok(invalid("the secondary opening amount cannot be negative"));
    };
    let Some(currency) = text(payload, "currency") else {
        return Ok(invalid("cash_session_opened requires a currency"));
    };
    let Some(secondary_currency) = text(payload, "secondary_currency") else {
        return Ok(invalid("cash_session_opened requires a secondary currency"));
    };
    if currency.len() != 3 || secondary_currency.len() != 3 {
        return Ok(invalid("currencies must have three letters"));
    }
    if currency == secondary_currency {
        return Ok(invalid("the drawers must use different currencies"));
    }

    let already_open: bool = sqlx::query_scalar(
        "SELECT EXISTS (
            SELECT 1 FROM cash_session
            WHERE branch_id = ?1 AND device_id = ?2 AND status = 'open'
         )",
    )
    .bind(&branch_id)
    .bind(&device_id)
    .fetch_one(&mut **transaction)
    .await?;
    if already_open {
        return Ok(invalid("the device already has an open cash session"));
    }

    let opened_by = optional_user(transaction, payload, "opened_by").await?;
    let opened_at_ms = integer(payload, "opened_at_ms").unwrap_or(operation.created_at_ms);

    sqlx::query(
        "INSERT INTO cash_session (
            id, business_id, branch_id, device_id, opened_by,
            opening_amount_minor, currency, secondary_currency,
            secondary_opening_amount_minor, status, opened_at_ms, notes
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'open', ?10, ?11)",
    )
    .bind(&session_id)
    .bind(&business_id)
    .bind(&branch_id)
    .bind(&device_id)
    .bind(opened_by.as_deref())
    .bind(opening_amount_minor)
    .bind(&currency)
    .bind(&secondary_currency)
    .bind(secondary_opening_amount_minor)
    .bind(opened_at_ms)
    .bind(optional_text(payload, "notes"))
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn cash_session_closed(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let session_id = operation.entity_id.to_string();

    let status: Option<String> =
        sqlx::query_scalar("SELECT status FROM cash_session WHERE id = ?1")
            .bind(&session_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some(status) = status else {
        return Ok(invalid("the cash session does not exist"));
    };
    if status == "closed" {
        return Ok(ProjectionOutcome::Applied);
    }

    let Some(expected_amount_minor) =
        integer(payload, "expected_amount_minor").filter(|value| *value >= 0)
    else {
        return Ok(invalid("the expected amount cannot be negative"));
    };
    let Some(counted_amount_minor) =
        integer(payload, "counted_amount_minor").filter(|value| *value >= 0)
    else {
        return Ok(invalid("the counted amount cannot be negative"));
    };
    let difference_minor = integer(payload, "difference_minor")
        .unwrap_or(counted_amount_minor - expected_amount_minor);
    let secondary_expected_amount_minor =
        integer(payload, "secondary_expected_amount_minor").unwrap_or(0);
    let secondary_counted_amount_minor =
        integer(payload, "secondary_counted_amount_minor").unwrap_or(0);
    let secondary_difference_minor = integer(payload, "secondary_difference_minor")
        .unwrap_or(secondary_counted_amount_minor - secondary_expected_amount_minor);
    let closed_at_ms = integer(payload, "closed_at_ms").unwrap_or(operation.created_at_ms);
    let closed_by = optional_user(transaction, payload, "closed_by").await?;

    sqlx::query(
        "UPDATE cash_session
         SET status = 'closed',
             closed_by = ?2,
             closed_at_ms = ?3,
             expected_amount_minor = ?4,
             secondary_expected_amount_minor = ?5,
             counted_amount_minor = ?6,
             secondary_counted_amount_minor = ?7,
             difference_minor = ?8,
             secondary_difference_minor = ?9,
             notes = COALESCE(?10, notes)
         WHERE id = ?1 AND status = 'open'",
    )
    .bind(&session_id)
    .bind(closed_by.as_deref())
    .bind(closed_at_ms)
    .bind(expected_amount_minor)
    .bind(secondary_expected_amount_minor)
    .bind(counted_amount_minor)
    .bind(secondary_counted_amount_minor)
    .bind(difference_minor)
    .bind(secondary_difference_minor)
    .bind(optional_text(payload, "notes"))
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn cash_movement_recorded(
    transaction: &mut Transaction<'_, Sqlite>,
    operation: &Operation,
) -> Result<ProjectionOutcome, AppError> {
    let payload = &operation.payload;
    let movement_id = operation.entity_id.to_string();

    let already_exists: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM cash_movement WHERE id = ?1)")
            .bind(&movement_id)
            .fetch_one(&mut **transaction)
            .await?;
    if already_exists {
        return Ok(ProjectionOutcome::Applied);
    }

    let Some(session_id) = text(payload, "cash_session_id") else {
        return Ok(invalid("cash_movement_recorded requires a cash session"));
    };
    let business_id: Option<String> =
        sqlx::query_scalar("SELECT business_id FROM cash_session WHERE id = ?1")
            .bind(&session_id)
            .fetch_optional(&mut **transaction)
            .await?;
    let Some(business_id) = business_id else {
        return Ok(invalid("the cash session does not exist"));
    };
    let Some(kind) = text(payload, "kind") else {
        return Ok(invalid("cash_movement_recorded requires a kind"));
    };
    if !matches!(
        kind.as_str(),
        "in" | "out" | "sale" | "refund" | "expense" | "withdrawal" | "deposit"
    ) {
        return Ok(invalid("invalid cash movement kind"));
    }
    let Some(amount_minor) = integer(payload, "amount_minor").filter(|value| *value > 0) else {
        return Ok(invalid("cash movement amounts must be greater than zero"));
    };
    let Some(currency) = text(payload, "currency") else {
        return Ok(invalid("cash_movement_recorded requires a currency"));
    };
    if currency.len() != 3 {
        return Ok(invalid("currency must have three letters"));
    }

    let device_id = resolve_device(transaction, payload, operation.device_id).await?;
    if !device_exists(transaction, &device_id).await? {
        return Ok(invalid("the device does not exist"));
    }
    let created_by = optional_user(transaction, payload, "created_by").await?;
    let created_at_ms = integer(payload, "created_at_ms").unwrap_or(operation.created_at_ms);

    sqlx::query(
        "INSERT INTO cash_movement (
            id, business_id, cash_session_id, kind, amount_minor, currency,
            reason, reference_type, reference_id, created_at_ms, created_by,
            device_id
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
    )
    .bind(&movement_id)
    .bind(&business_id)
    .bind(&session_id)
    .bind(&kind)
    .bind(amount_minor)
    .bind(&currency)
    .bind(optional_text(payload, "reason"))
    .bind(optional_text(payload, "reference_type"))
    .bind(optional_text(payload, "reference_id"))
    .bind(created_at_ms)
    .bind(created_by.as_deref())
    .bind(&device_id)
    .execute(&mut **transaction)
    .await?;

    Ok(ProjectionOutcome::Applied)
}

async fn device_exists(
    transaction: &mut Transaction<'_, Sqlite>,
    device_id: &str,
) -> Result<bool, AppError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM device WHERE id = ?1)")
            .bind(device_id)
            .fetch_one(&mut **transaction)
            .await?,
    )
}

/// Returns the device referenced by the payload when it exists, otherwise the
/// device that authored the operation.
async fn resolve_device(
    transaction: &mut Transaction<'_, Sqlite>,
    payload: &Value,
    fallback: nexo_core::DeviceId,
) -> Result<String, AppError> {
    if let Some(device_id) = optional_text(payload, "device_id")
        && device_exists(transaction, &device_id).await?
    {
        return Ok(device_id);
    }

    Ok(fallback.to_string())
}

async fn optional_user(
    transaction: &mut Transaction<'_, Sqlite>,
    payload: &Value,
    field: &str,
) -> Result<Option<String>, AppError> {
    let Some(user_id) = optional_text(payload, field) else {
        return Ok(None);
    };

    if user_exists(transaction, &user_id).await? {
        Ok(Some(user_id))
    } else {
        Ok(None)
    }
}

async fn record_order_event(
    transaction: &mut Transaction<'_, Sqlite>,
    order_id: &str,
    kind: &str,
    actor: Option<&str>,
    operation: &Operation,
    details: Value,
    at: i64,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO order_event (
            id, order_id, kind, actor_user_id, device_id, details, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(order_id)
    .bind(kind)
    .bind(actor)
    .bind(operation.device_id.to_string())
    .bind(details.to_string())
    .bind(at)
    .execute(&mut **transaction)
    .await?;

    Ok(())
}

async fn user_exists(
    transaction: &mut Transaction<'_, Sqlite>,
    user_id: &str,
) -> Result<bool, AppError> {
    Ok(
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM app_user WHERE id = ?1)")
            .bind(user_id)
            .fetch_one(&mut **transaction)
            .await?,
    )
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

fn string_array(payload: &Value, field: &str) -> Option<Vec<String>> {
    payload.get(field).and_then(Value::as_array).map(|values| {
        values
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect()
    })
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

#[derive(Debug, Clone)]
pub struct ProjectedOrder {
    pub id: String,
    pub order_type: String,
    pub table_id: Option<String>,
    pub table_name: Option<String>,
    pub customer_name: Option<String>,
    pub display_code: String,
    pub status: String,
    pub total_minor: i64,
    pub pending_lines: i64,
    pub kitchen_lines: i64,
    pub opened_at_ms: i64,
}

#[derive(Debug, Clone)]
pub struct ProjectedOrderLine {
    pub id: String,
    pub description: String,
    pub quantity: i64,
    pub unit_price_minor: i64,
    pub line_total_minor: i64,
    pub currency: String,
    pub notes: Option<String>,
    pub status: String,
    pub round_number: i64,
    pub sent_at_ms: Option<i64>,
    pub void_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ProjectedOrderDetail {
    pub order: ProjectedOrder,
    pub lines: Vec<ProjectedOrderLine>,
}

/// Lists open, active, and served orders with their derived totals.
pub async fn open_orders(
    pool: &sqlx::SqlitePool,
    business_id: &str,
) -> Result<Vec<ProjectedOrder>, AppError> {
    let rows = sqlx::query(
        "SELECT
            o.id, o.order_type, o.table_id, t.name AS table_name,
            o.customer_name, o.display_code, o.status, o.opened_at_ms,
            COALESCE(SUM(
                CASE WHEN l.status <> 'voided' THEN l.line_total_minor ELSE 0 END
            ), 0) AS total_minor,
            COUNT(CASE WHEN l.status = 'pending' THEN 1 END) AS pending_lines,
            COUNT(CASE WHEN l.status IN ('sent', 'ready') THEN 1 END) AS kitchen_lines
         FROM restaurant_order o
         LEFT JOIN dining_table t ON t.id = o.table_id
         LEFT JOIN order_line l ON l.order_id = o.id
         WHERE o.business_id = ?1 AND o.status IN ('open', 'active', 'served')
         GROUP BY o.id
         ORDER BY o.opened_at_ms, o.id",
    )
    .bind(business_id)
    .fetch_all(pool)
    .await?;

    rows.iter()
        .map(projected_order_from_row)
        .collect::<Result<Vec<_>, AppError>>()
}

/// Loads one order with its lines, including billed and cancelled orders.
pub async fn order_detail(
    pool: &sqlx::SqlitePool,
    business_id: &str,
    order_id: &str,
) -> Result<Option<ProjectedOrderDetail>, AppError> {
    let row = sqlx::query(
        "SELECT
            o.id, o.order_type, o.table_id, t.name AS table_name,
            o.customer_name, o.display_code, o.status, o.opened_at_ms,
            COALESCE(SUM(
                CASE WHEN l.status <> 'voided' THEN l.line_total_minor ELSE 0 END
            ), 0) AS total_minor,
            COUNT(CASE WHEN l.status = 'pending' THEN 1 END) AS pending_lines,
            COUNT(CASE WHEN l.status IN ('sent', 'ready') THEN 1 END) AS kitchen_lines
         FROM restaurant_order o
         LEFT JOIN dining_table t ON t.id = o.table_id
         LEFT JOIN order_line l ON l.order_id = o.id
         WHERE o.business_id = ?1 AND o.id = ?2
         GROUP BY o.id",
    )
    .bind(business_id)
    .bind(order_id)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let order = projected_order_from_row(&row)?;
    let line_rows = sqlx::query(
        "SELECT
            id, description, quantity, unit_price_minor, line_total_minor,
            currency, notes, status, round_number, sent_at_ms, void_reason
         FROM order_line
         WHERE order_id = ?1
         ORDER BY round_number, created_at_ms, id",
    )
    .bind(order_id)
    .fetch_all(pool)
    .await?;

    let lines = line_rows
        .iter()
        .map(projected_line_from_row)
        .collect::<Result<Vec<_>, AppError>>()?;

    Ok(Some(ProjectedOrderDetail { order, lines }))
}

/// Lists orders with lines that the kitchen has not finished, oldest first.
pub async fn kitchen_orders(
    pool: &sqlx::SqlitePool,
    business_id: &str,
) -> Result<Vec<ProjectedOrderDetail>, AppError> {
    let rows = sqlx::query(
        "SELECT
            o.id, o.order_type, o.table_id, t.name AS table_name,
            o.customer_name, o.display_code, o.status, o.opened_at_ms,
            COALESCE(SUM(
                CASE WHEN l.status <> 'voided' THEN l.line_total_minor ELSE 0 END
            ), 0) AS total_minor,
            COUNT(CASE WHEN l.status = 'pending' THEN 1 END) AS pending_lines,
            COUNT(CASE WHEN l.status IN ('sent', 'ready') THEN 1 END) AS kitchen_lines
         FROM restaurant_order o
         LEFT JOIN dining_table t ON t.id = o.table_id
         LEFT JOIN order_line l ON l.order_id = o.id
         WHERE o.business_id = ?1
           AND o.status IN ('open', 'active', 'served')
           AND EXISTS (
                SELECT 1 FROM order_line k
                WHERE k.order_id = o.id AND k.status IN ('sent', 'ready')
           )
         GROUP BY o.id
         ORDER BY o.opened_at_ms, o.id",
    )
    .bind(business_id)
    .fetch_all(pool)
    .await?;

    let mut orders = Vec::with_capacity(rows.len());

    for row in &rows {
        let order = projected_order_from_row(row)?;
        let line_rows = sqlx::query(
            "SELECT
                id, description, quantity, unit_price_minor, line_total_minor,
                currency, notes, status, round_number, sent_at_ms, void_reason
             FROM order_line
             WHERE order_id = ?1 AND status IN ('sent', 'ready')
             ORDER BY round_number, created_at_ms, id",
        )
        .bind(&order.id)
        .fetch_all(pool)
        .await?;

        let lines = line_rows
            .iter()
            .map(projected_line_from_row)
            .collect::<Result<Vec<_>, AppError>>()?;

        orders.push(ProjectedOrderDetail { order, lines });
    }

    Ok(orders)
}

fn projected_order_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ProjectedOrder, AppError> {
    Ok(ProjectedOrder {
        id: row.try_get("id")?,
        order_type: row.try_get("order_type")?,
        table_id: row.try_get("table_id")?,
        table_name: row.try_get("table_name")?,
        customer_name: row.try_get("customer_name")?,
        display_code: row.try_get("display_code")?,
        status: row.try_get("status")?,
        total_minor: row.try_get("total_minor")?,
        pending_lines: row.try_get("pending_lines")?,
        kitchen_lines: row.try_get("kitchen_lines")?,
        opened_at_ms: row.try_get("opened_at_ms")?,
    })
}

fn projected_line_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<ProjectedOrderLine, AppError> {
    Ok(ProjectedOrderLine {
        id: row.try_get("id")?,
        description: row.try_get("description")?,
        quantity: row.try_get("quantity")?,
        unit_price_minor: row.try_get("unit_price_minor")?,
        line_total_minor: row.try_get("line_total_minor")?,
        currency: row.try_get("currency")?,
        notes: row.try_get("notes")?,
        status: row.try_get("status")?,
        round_number: row.try_get("round_number")?,
        sent_at_ms: row.try_get("sent_at_ms")?,
        void_reason: row.try_get("void_reason")?,
    })
}

#[cfg(test)]
mod tests {
    use nexo_core::{BranchId, DeviceId, Operation, OperationKind, Uuid};
    use serde_json::json;

    use super::*;
    use crate::state::AppState;
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

    async fn linked_order_context(
        state: &AppState,
    ) -> (DeviceId, BranchId, String, String, String) {
        let snapshot = crate::snapshot::sample_snapshot();
        let device_id = snapshot.device.id.parse::<DeviceId>().unwrap();
        let branch_id = snapshot.branch.id.parse::<BranchId>().unwrap();
        let table_id = snapshot.tables[0].id.clone();
        let item_id = snapshot.items[0].id.clone();
        let user_id = snapshot.users[0].id.clone();

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

        (device_id, branch_id, table_id, item_id, user_id)
    }

    #[tokio::test]
    async fn orders_fold_through_rounds_ready_and_served() {
        let state = test_support::state().await;
        let (device_id, branch_id, table_id, item_id, user_id) = linked_order_context(&state).await;
        let business = business_id(&state.pool).await;
        let order_id = Uuid::now_v7();
        let line_id = Uuid::now_v7();
        let at = 1_700_000_000_000;

        let order_operation = |kind, entity_id, payload| {
            Operation::new(device_id, Some(branch_id), kind, entity_id, payload, at)
        };

        sync::push(
            &state.pool,
            vec![
                order_operation(
                    OperationKind::OrderOpened,
                    order_id,
                    json!({
                        "order_type": "dine_in",
                        "table_id": table_id,
                        "display_code": "M-001",
                        "opened_by": user_id,
                        "opened_at_ms": at,
                    }),
                ),
                order_operation(
                    OperationKind::OrderLineAdded,
                    line_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "menu_item_id": item_id,
                        "description": "Arepa",
                        "quantity": 2,
                        "unit_price_minor": 250,
                        "line_total_minor": 500,
                        "currency": "USD",
                        "notes": "Sin queso",
                        "round_number": 1,
                        "created_by": user_id,
                        "created_at_ms": at + 1,
                    }),
                ),
                order_operation(
                    OperationKind::OrderSent,
                    order_id,
                    json!({
                        "round_number": 1,
                        "line_ids": [line_id.to_string()],
                        "sent_by": user_id,
                        "sent_at_ms": at + 2,
                    }),
                ),
                order_operation(
                    OperationKind::OrderLineReady,
                    line_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "ready_at_ms": at + 3,
                    }),
                ),
                order_operation(
                    OperationKind::OrderLineServed,
                    line_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "served_at_ms": at + 4,
                    }),
                ),
            ],
        )
        .await
        .unwrap();

        let orders = open_orders(&state.pool, &business).await.unwrap();
        assert_eq!(orders.len(), 1);
        assert_eq!(orders[0].status, "served");
        assert_eq!(orders[0].total_minor, 500);
        assert_eq!(orders[0].pending_lines, 0);
        assert_eq!(orders[0].kitchen_lines, 0);
        assert_eq!(orders[0].table_name.as_deref(), Some("Mesa 1"));

        let detail = order_detail(&state.pool, &business, &order_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.lines.len(), 1);
        assert_eq!(detail.lines[0].status, "served");
        assert_eq!(detail.lines[0].void_reason, None);

        assert!(
            kitchen_orders(&state.pool, &business)
                .await
                .unwrap()
                .is_empty()
        );

        let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM order_event")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(events, 5);
    }

    #[tokio::test]
    async fn a_second_open_order_for_a_table_is_an_anomaly() {
        let state = test_support::state().await;
        let (device_id, branch_id, table_id, _, user_id) = linked_order_context(&state).await;
        let at = 1_700_000_000_000;

        let opened = |order_id: Uuid, code: &str| {
            Operation::new(
                device_id,
                Some(branch_id),
                OperationKind::OrderOpened,
                order_id,
                json!({
                    "order_type": "dine_in",
                    "table_id": table_id,
                    "display_code": code,
                    "opened_by": user_id,
                    "opened_at_ms": at,
                }),
                at,
            )
        };

        sync::push(
            &state.pool,
            vec![
                opened(Uuid::now_v7(), "M-001"),
                opened(Uuid::now_v7(), "M-002"),
            ],
        )
        .await
        .unwrap();

        let orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM restaurant_order")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let anomalies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_anomaly")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        assert_eq!(orders, 1);
        assert_eq!(anomalies, 1);
    }

    #[tokio::test]
    async fn orders_can_share_a_display_code_after_concurrent_creation() {
        let state = test_support::state().await;
        let (device_id, branch_id, _, _, user_id) = linked_order_context(&state).await;
        let at = 1_700_000_000_000;

        let opened = |order_id: Uuid| {
            Operation::new(
                device_id,
                Some(branch_id),
                OperationKind::OrderOpened,
                order_id,
                json!({
                    "order_type": "takeout",
                    "display_code": "L-001",
                    "opened_by": user_id,
                    "opened_at_ms": at,
                }),
                at,
            )
        };

        sync::push(
            &state.pool,
            vec![opened(Uuid::now_v7()), opened(Uuid::now_v7())],
        )
        .await
        .unwrap();

        let orders: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM restaurant_order")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let anomalies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_anomaly")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        assert_eq!(orders, 2);
        assert_eq!(anomalies, 0);
    }

    #[tokio::test]
    async fn invalid_order_transitions_are_anomalies() {
        let state = test_support::state().await;
        let (device_id, branch_id, table_id, item_id, user_id) = linked_order_context(&state).await;
        let business = business_id(&state.pool).await;
        let order_id = Uuid::now_v7();
        let line_id = Uuid::now_v7();
        let at = 1_700_000_000_000;

        let order_operation = |kind, entity_id, payload| {
            Operation::new(device_id, Some(branch_id), kind, entity_id, payload, at)
        };

        sync::push(
            &state.pool,
            vec![
                order_operation(
                    OperationKind::OrderOpened,
                    order_id,
                    json!({
                        "order_type": "dine_in",
                        "table_id": table_id,
                        "display_code": "M-001",
                        "opened_by": user_id,
                        "opened_at_ms": at,
                    }),
                ),
                order_operation(
                    OperationKind::OrderLineAdded,
                    line_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "menu_item_id": item_id,
                        "description": "Arepa",
                        "quantity": 1,
                        "unit_price_minor": 250,
                        "line_total_minor": 250,
                        "currency": "USD",
                        "round_number": 1,
                        "created_by": user_id,
                        "created_at_ms": at + 1,
                    }),
                ),
                order_operation(
                    OperationKind::OrderLineReady,
                    line_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "ready_at_ms": at + 2,
                    }),
                ),
            ],
        )
        .await
        .unwrap();

        let detail = order_detail(&state.pool, &business, &order_id.to_string())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(detail.lines[0].status, "pending");

        let anomalies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_anomaly")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(anomalies, 1);
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

    #[tokio::test]
    async fn billing_folds_sale_payments_and_cash_in_any_pair_order() {
        let state = test_support::state().await;
        let (device_id, branch_id, table_id, item_id, user_id) = linked_order_context(&state).await;
        let method_id: String = sqlx::query_scalar("SELECT id FROM payment_method LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let order_id = Uuid::now_v7();
        let line_id = Uuid::now_v7();
        let session_id = Uuid::now_v7();
        let sale_id = Uuid::now_v7();
        let payment_id = Uuid::now_v7();
        let movement_id = Uuid::now_v7();
        let at = 1_700_000_000_000;

        let billing_operation = |kind, entity_id, payload| {
            Operation::new(device_id, Some(branch_id), kind, entity_id, payload, at)
        };

        sync::push(
            &state.pool,
            vec![
                billing_operation(
                    OperationKind::OrderOpened,
                    order_id,
                    json!({
                        "order_type": "dine_in",
                        "table_id": table_id,
                        "display_code": "M-001",
                        "opened_by": user_id,
                        "opened_at_ms": at,
                    }),
                ),
                billing_operation(
                    OperationKind::OrderLineAdded,
                    line_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "menu_item_id": item_id,
                        "description": "Arepa",
                        "quantity": 2,
                        "unit_price_minor": 250,
                        "line_total_minor": 500,
                        "currency": "USD",
                        "round_number": 1,
                        "created_by": user_id,
                        "created_at_ms": at + 1,
                    }),
                ),
                billing_operation(
                    OperationKind::CashSessionOpened,
                    session_id,
                    json!({
                        "cash_session_id": session_id.to_string(),
                        "branch_id": branch_id.to_string(),
                        "device_id": device_id.to_string(),
                        "opened_by": user_id,
                        "opening_amount_minor": 0,
                        "currency": "USD",
                        "secondary_currency": "VES",
                        "secondary_opening_amount_minor": 0,
                        "opened_at_ms": at + 2,
                    }),
                ),
                // `order_billed` arrives before `sale_created`: the pair is
                // commutative and both orders must produce the same state.
                billing_operation(
                    OperationKind::OrderBilled,
                    order_id,
                    json!({
                        "sale_id": sale_id.to_string(),
                        "number": "V-0001",
                        "subtotal_minor": 500,
                        "tax_minor": 0,
                        "total_minor": 500,
                        "currency": "USD",
                        "cash_session_id": session_id.to_string(),
                        "created_by": user_id,
                        "created_at_ms": at + 3,
                    }),
                ),
                billing_operation(
                    OperationKind::SaleCreated,
                    sale_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "number": "V-0001",
                        "subtotal_minor": 500,
                        "tax_minor": 0,
                        "total_minor": 500,
                        "currency": "USD",
                        "tax_name": null,
                        "tax_rate_basis_points": null,
                        "cash_session_id": session_id.to_string(),
                        "lines": [{
                            "line_id": line_id.to_string(),
                            "order_line_id": line_id.to_string(),
                            "menu_item_id": item_id,
                            "description": "Arepa",
                            "quantity": 2,
                            "unit_price_minor": 250,
                            "line_total_minor": 500,
                            "currency": "USD",
                        }],
                        "payments": [{
                            "payment_id": payment_id.to_string(),
                            "method_id": method_id,
                            "amount_minor": 500,
                            "currency": "USD",
                            "base_amount_minor": 500,
                            "exchange_rate_micros": null,
                            "reference": null,
                            "created_at_ms": at + 3,
                            "created_by": user_id,
                            "device_id": device_id.to_string(),
                        }],
                        "created_by": user_id,
                        "created_at_ms": at + 3,
                    }),
                ),
                billing_operation(
                    OperationKind::CashMovementRecorded,
                    movement_id,
                    json!({
                        "cash_movement_id": movement_id.to_string(),
                        "cash_session_id": session_id.to_string(),
                        "kind": "sale",
                        "amount_minor": 500,
                        "currency": "USD",
                        "reference_type": "sale",
                        "reference_id": sale_id.to_string(),
                        "created_by": user_id,
                        "created_at_ms": at + 4,
                    }),
                ),
                billing_operation(
                    OperationKind::CashSessionClosed,
                    session_id,
                    json!({
                        "cash_session_id": session_id.to_string(),
                        "closed_by": user_id,
                        "expected_amount_minor": 500,
                        "counted_amount_minor": 500,
                        "difference_minor": 0,
                        "secondary_expected_amount_minor": 0,
                        "secondary_counted_amount_minor": 0,
                        "secondary_difference_minor": 0,
                        "closed_at_ms": at + 5,
                    }),
                ),
            ],
        )
        .await
        .unwrap();

        let order_status: String =
            sqlx::query_scalar("SELECT status FROM restaurant_order WHERE id = ?1")
                .bind(order_id.to_string())
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(order_status, "billed");

        let sale: (String, i64, i64, String) =
            sqlx::query_as("SELECT number, total_minor, tax_minor, status FROM sale WHERE id = ?1")
                .bind(sale_id.to_string())
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(sale, ("V-0001".to_owned(), 500, 0, "completed".to_owned()));

        let lines: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sale_line WHERE sale_id = ?1")
            .bind(sale_id.to_string())
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let payments: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM payment WHERE sale_id = ?1")
            .bind(sale_id.to_string())
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(lines, 1);
        assert_eq!(payments, 1);

        let session: (String, i64, i64, i64) = sqlx::query_as(
            "SELECT status, expected_amount_minor, counted_amount_minor, difference_minor
             FROM cash_session WHERE id = ?1",
        )
        .bind(session_id.to_string())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(session, ("closed".to_owned(), 500, 500, 0));

        let movement_kind: String =
            sqlx::query_scalar("SELECT kind FROM cash_movement WHERE id = ?1")
                .bind(movement_id.to_string())
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(movement_kind, "sale");

        let billed_events: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM order_event WHERE order_id = ?1 AND kind = 'order_billed'",
        )
        .bind(order_id.to_string())
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(billed_events, 1);

        let anomalies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_anomaly")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        assert_eq!(anomalies, 0);
    }

    #[tokio::test]
    async fn a_cancelled_order_cannot_be_billed() {
        let state = test_support::state().await;
        let (device_id, branch_id, table_id, _, user_id) = linked_order_context(&state).await;
        let method_id: String = sqlx::query_scalar("SELECT id FROM payment_method LIMIT 1")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let order_id = Uuid::now_v7();
        let sale_id = Uuid::now_v7();
        let payment_id = Uuid::now_v7();
        let at = 1_700_000_000_000;

        let billing_operation = |kind, entity_id, payload| {
            Operation::new(device_id, Some(branch_id), kind, entity_id, payload, at)
        };

        sync::push(
            &state.pool,
            vec![
                billing_operation(
                    OperationKind::OrderOpened,
                    order_id,
                    json!({
                        "order_type": "dine_in",
                        "table_id": table_id,
                        "display_code": "M-001",
                        "opened_by": user_id,
                        "opened_at_ms": at,
                    }),
                ),
                billing_operation(
                    OperationKind::OrderCancelled,
                    order_id,
                    json!({
                        "reason": "Cliente se retiró",
                        "cancelled_by": user_id,
                        "cancelled_at_ms": at + 1,
                    }),
                ),
                billing_operation(
                    OperationKind::SaleCreated,
                    sale_id,
                    json!({
                        "order_id": order_id.to_string(),
                        "number": "V-0001",
                        "subtotal_minor": 500,
                        "tax_minor": 0,
                        "total_minor": 500,
                        "currency": "USD",
                        "lines": [{
                            "line_id": Uuid::now_v7().to_string(),
                            "description": "Arepa",
                            "quantity": 2,
                            "unit_price_minor": 250,
                            "line_total_minor": 500,
                            "currency": "USD",
                        }],
                        "payments": [{
                            "payment_id": payment_id.to_string(),
                            "method_id": method_id,
                            "amount_minor": 500,
                            "currency": "USD",
                            "base_amount_minor": 500,
                            "created_at_ms": at + 2,
                            "created_by": user_id,
                            "device_id": device_id.to_string(),
                        }],
                        "created_by": user_id,
                        "created_at_ms": at + 2,
                    }),
                ),
            ],
        )
        .await
        .unwrap();

        let sales: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sale")
            .fetch_one(&state.pool)
            .await
            .unwrap();
        let anomalies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_anomaly")
            .fetch_one(&state.pool)
            .await
            .unwrap();

        assert_eq!(sales, 0);
        assert_eq!(anomalies, 1);
    }
}
