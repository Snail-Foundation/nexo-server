use nexo_core::{Operation, Uuid};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::{Row, SqlitePool};

use crate::error::AppError;
use crate::projection::{self, ProjectionOutcome};
use crate::time::now_ms;

pub const DEFAULT_PULL_LIMIT: i64 = 200;
pub const MAX_PULL_LIMIT: i64 = 500;

#[derive(Debug, Deserialize)]
pub struct PushRequest {
    pub operations: Vec<Operation>,
}

#[derive(Debug, Serialize)]
pub struct PushResponse {
    pub accepted: usize,
    pub duplicates: usize,
    pub last_seq: i64,
}

#[derive(Debug, Serialize)]
pub struct SequencedOperation {
    pub seq: i64,
    pub operation: Operation,
}

#[derive(Debug, Serialize)]
pub struct PullResponse {
    pub operations: Vec<SequencedOperation>,
    pub next: i64,
    pub has_more: bool,
}

/// Validates and appends a batch of operations.
///
/// Operation IDs are globally unique, so replaying a batch only inserts the
/// operations that were not stored before. The whole batch is atomic: if a
/// write fails, no operation from the batch is committed.
pub async fn push(pool: &SqlitePool, operations: Vec<Operation>) -> Result<PushResponse, AppError> {
    if operations.is_empty() {
        return Ok(PushResponse {
            accepted: 0,
            duplicates: 0,
            last_seq: last_seq(pool).await?,
        });
    }

    let mut invalid = Vec::new();

    for operation in &operations {
        if let Err(error) = operation.validate() {
            invalid.push((operation, error.to_string()));
            continue;
        }

        if operation.schema_version > Operation::CURRENT_SCHEMA_VERSION {
            invalid.push((
                operation,
                format!("unsupported schema version: {}", operation.schema_version),
            ));
        }
    }

    if !invalid.is_empty() {
        for (operation, message) in invalid {
            record_anomaly(
                pool,
                Some(&operation.id.to_string()),
                Some(&operation.device_id.to_string()),
                "invalid_operation",
                json!({ "error": message }),
            )
            .await?;
        }

        return Err(AppError::Validation(
            "one or more operations are invalid".to_owned(),
        ));
    }

    let mut transaction = pool.begin().await?;
    let mut accepted = 0usize;
    let mut duplicates = 0usize;
    let received_at_ms = now_ms();

    for operation in &operations {
        let payload = serde_json::to_string(&operation.payload)?;

        let result = sqlx::query(
            "INSERT INTO operation_log (
                id, device_id, branch_id, kind, entity_id, payload,
                schema_version, created_at_ms, received_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT (id) DO NOTHING",
        )
        .bind(operation.id.to_string())
        .bind(operation.device_id.to_string())
        .bind(operation.branch_id.map(|branch_id| branch_id.to_string()))
        .bind(operation.kind.as_str())
        .bind(operation.entity_id.to_string())
        .bind(payload)
        .bind(i64::from(operation.schema_version))
        .bind(operation.created_at_ms)
        .bind(received_at_ms)
        .execute(&mut *transaction)
        .await?;

        if result.rows_affected() == 0 {
            duplicates += 1;
        } else {
            accepted += 1;

            match projection::apply(&mut transaction, operation).await? {
                ProjectionOutcome::Applied | ProjectionOutcome::Ignored => {}
                ProjectionOutcome::Skipped(reason) => {
                    sqlx::query(
                        "INSERT INTO sync_anomaly (
                            id, operation_id, device_id, kind, details, created_at_ms
                         ) VALUES (?1, ?2, ?3, 'projection_skipped', ?4, ?5)",
                    )
                    .bind(Uuid::now_v7().to_string())
                    .bind(operation.id.to_string())
                    .bind(operation.device_id.to_string())
                    .bind(json!({ "reason": reason }).to_string())
                    .bind(received_at_ms)
                    .execute(&mut *transaction)
                    .await?;
                }
            }
        }
    }

    transaction.commit().await?;

    Ok(PushResponse {
        accepted,
        duplicates,
        last_seq: last_seq(pool).await?,
    })
}

/// Returns operations after `since`, in server sequence order.
pub async fn pull(pool: &SqlitePool, since: i64, limit: i64) -> Result<PullResponse, AppError> {
    if since < 0 {
        return Err(AppError::Validation(
            "since must be zero or greater".to_owned(),
        ));
    }

    let limit = if limit <= 0 {
        DEFAULT_PULL_LIMIT
    } else {
        limit.min(MAX_PULL_LIMIT)
    };

    let rows = sqlx::query(
        "SELECT
            seq, id, device_id, branch_id, kind, entity_id, payload,
            schema_version, created_at_ms
         FROM operation_log
         WHERE seq > ?1
         ORDER BY seq
         LIMIT ?2",
    )
    .bind(since)
    .bind(limit + 1)
    .fetch_all(pool)
    .await?;

    let has_more = rows.len() as i64 > limit;
    let mut operations = Vec::with_capacity(rows.len().min(limit as usize));
    let mut next = since;

    for row in rows.into_iter().take(limit as usize) {
        let seq: i64 = row.try_get("seq")?;
        next = seq;
        operations.push(SequencedOperation {
            seq,
            operation: operation_from_row(&row)?,
        });
    }

    Ok(PullResponse {
        operations,
        next,
        has_more,
    })
}

/// Appends an operation written by the web app, applies its projection, and
/// records an audit event in one transaction.
pub async fn append_web_operation(
    pool: &SqlitePool,
    operation: &Operation,
    actor_user_id: &str,
    business_id: &str,
    branch_id: Option<&str>,
    action: &str,
    details: Value,
) -> Result<(), AppError> {
    operation
        .validate()
        .map_err(|error| AppError::Validation(error.to_string()))?;

    let payload = serde_json::to_string(&operation.payload)?;
    let now = now_ms();
    let mut transaction = pool.begin().await?;

    sqlx::query(
        "INSERT INTO operation_log (
            id, device_id, branch_id, kind, entity_id, payload,
            schema_version, created_at_ms, received_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(operation.id.to_string())
    .bind(operation.device_id.to_string())
    .bind(operation.branch_id.map(|branch_id| branch_id.to_string()))
    .bind(operation.kind.as_str())
    .bind(operation.entity_id.to_string())
    .bind(payload)
    .bind(i64::from(operation.schema_version))
    .bind(operation.created_at_ms)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    match projection::apply(&mut transaction, operation).await? {
        ProjectionOutcome::Applied | ProjectionOutcome::Ignored => {}
        ProjectionOutcome::Skipped(reason) => {
            return Err(AppError::Validation(reason));
        }
    }

    sqlx::query(
        "INSERT INTO audit_event (
            id, business_id, branch_id, actor_user_id, device_id, action,
            entity_type, entity_id, details, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(business_id)
    .bind(branch_id)
    .bind(actor_user_id)
    .bind(operation.device_id.to_string())
    .bind(action)
    .bind(operation.kind.as_str())
    .bind(operation.entity_id.to_string())
    .bind(serde_json::to_string(&details)?)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;

    Ok(())
}

/// Records a rejected or conflicting operation for human review.
pub async fn record_anomaly(
    pool: &SqlitePool,
    operation_id: Option<&str>,
    device_id: Option<&str>,
    kind: &str,
    details: Value,
) -> Result<(), AppError> {
    sqlx::query(
        "INSERT INTO sync_anomaly (
            id, operation_id, device_id, kind, details, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )
    .bind(Uuid::now_v7().to_string())
    .bind(operation_id)
    .bind(device_id)
    .bind(kind)
    .bind(serde_json::to_string(&details)?)
    .bind(now_ms())
    .execute(pool)
    .await?;

    Ok(())
}

async fn last_seq(pool: &SqlitePool) -> Result<i64, AppError> {
    let last_seq: i64 = sqlx::query_scalar("SELECT COALESCE(MAX(seq), 0) FROM operation_log")
        .fetch_one(pool)
        .await?;

    Ok(last_seq)
}

fn operation_from_row(row: &sqlx::sqlite::SqliteRow) -> Result<Operation, AppError> {
    let id: String = row.try_get("id")?;
    let device_id: String = row.try_get("device_id")?;
    let branch_id: Option<String> = row.try_get("branch_id")?;
    let kind: String = row.try_get("kind")?;
    let entity_id: String = row.try_get("entity_id")?;
    let payload: String = row.try_get("payload")?;
    let schema_version: i64 = row.try_get("schema_version")?;

    let parse_error =
        |field: &str, error: String| AppError::CorruptData(format!("invalid {field}: {error}"));

    Ok(Operation {
        id: id
            .parse()
            .map_err(|error| parse_error("operation id", format!("{error:?}")))?,
        device_id: device_id
            .parse()
            .map_err(|error| parse_error("device id", format!("{error:?}")))?,
        branch_id: branch_id
            .map(|value| value.parse())
            .transpose()
            .map_err(|error| parse_error("branch id", format!("{error:?}")))?,
        kind: kind
            .parse()
            .map_err(|error| parse_error("operation kind", format!("{error:?}")))?,
        entity_id: entity_id
            .parse()
            .map_err(|error| parse_error("entity id", format!("{error:?}")))?,
        payload: serde_json::from_str(&payload)?,
        created_at_ms: row.try_get("created_at_ms")?,
        schema_version: u16::try_from(schema_version)
            .map_err(|error| parse_error("schema version", error.to_string()))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use nexo_core::{DeviceId, OperationKind};

    async fn pool() -> SqlitePool {
        let pool = db::connect_in_memory().await.unwrap();
        db::migrate(&pool).await.unwrap();
        pool
    }

    fn operation(kind: OperationKind) -> Operation {
        Operation::new(
            DeviceId::new(),
            None,
            kind,
            Uuid::now_v7(),
            json!({ "test": true }),
            1_700_000_000_000,
        )
    }

    #[tokio::test]
    async fn push_appends_and_pull_returns_in_order() {
        let pool = pool().await;
        let first = operation(OperationKind::OrderOpened);
        let second = operation(OperationKind::OrderLineAdded);

        let response = push(&pool, vec![first.clone(), second.clone()])
            .await
            .unwrap();

        assert_eq!(response.accepted, 2);
        assert_eq!(response.duplicates, 0);
        assert_eq!(response.last_seq, 2);

        let pulled = pull(&pool, 0, 100).await.unwrap();

        assert_eq!(pulled.operations.len(), 2);
        assert_eq!(pulled.operations[0].seq, 1);
        assert_eq!(pulled.operations[0].operation.id, first.id);
        assert_eq!(pulled.operations[1].seq, 2);
        assert_eq!(pulled.operations[1].operation.id, second.id);
        assert_eq!(pulled.next, 2);
        assert!(!pulled.has_more);
    }

    #[tokio::test]
    async fn replaying_an_operation_is_idempotent() {
        let pool = pool().await;
        let operation = operation(OperationKind::OrderOpened);

        let first = push(&pool, vec![operation.clone()]).await.unwrap();
        let second = push(&pool, vec![operation.clone()]).await.unwrap();

        assert_eq!(first.accepted, 1);
        assert_eq!(second.accepted, 0);
        assert_eq!(second.duplicates, 1);
        assert_eq!(second.last_seq, 1);

        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM operation_log")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn pull_paginates_without_skipping_operations() {
        let pool = pool().await;
        let operations = (0..3)
            .map(|_| operation(OperationKind::OrderLineAdded))
            .collect();

        push(&pool, operations).await.unwrap();

        let first = pull(&pool, 0, 2).await.unwrap();
        assert_eq!(first.operations.len(), 2);
        assert_eq!(first.next, 2);
        assert!(first.has_more);

        let second = pull(&pool, first.next, 2).await.unwrap();
        assert_eq!(second.operations.len(), 1);
        assert_eq!(second.next, 3);
        assert!(!second.has_more);
    }

    #[tokio::test]
    async fn invalid_operations_are_rejected_and_recorded() {
        let pool = pool().await;
        let mut operation = operation(OperationKind::OrderOpened);
        operation.payload = Value::Null;

        let result = push(&pool, vec![operation]).await;

        assert!(matches!(result, Err(AppError::Validation(_))));

        let anomalies: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sync_anomaly")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(anomalies, 1);
    }
}
