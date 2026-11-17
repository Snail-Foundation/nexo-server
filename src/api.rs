use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};

use crate::auth::{self, SessionKind};
use crate::error::AppError;
use crate::snapshot;
use crate::state::AppState;
use crate::sync;

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub database: &'static str,
}

/// Liveness and database check.
pub async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, AppError> {
    sqlx::query_scalar::<_, i64>("SELECT 1")
        .fetch_one(&state.pool)
        .await?;

    Ok(Json(HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        database: "ok",
    }))
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub user: String,
    pub pin: String,
    #[serde(default)]
    pub device_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    pub token: String,
    pub expires_at_ms: i64,
    pub user: UserSummary,
    pub business: BusinessSummary,
}

#[derive(Debug, Serialize)]
pub struct UserSummary {
    pub id: String,
    pub name: String,
    pub role: String,
}

#[derive(Debug, Serialize)]
pub struct BusinessSummary {
    pub id: String,
    pub name: String,
}

/// Exchanges a user PIN for an API bearer token.
pub async fn login(
    State(state): State<AppState>,
    Json(request): Json<LoginRequest>,
) -> Result<Json<LoginResponse>, AppError> {
    let login = auth::login(
        &state.pool,
        &request.user,
        &request.pin,
        SessionKind::Api,
        request.device_id.as_deref(),
        state.config.session_hours,
    )
    .await?
    .ok_or(AppError::Unauthorized)?;

    Ok(Json(LoginResponse {
        token: login.token,
        expires_at_ms: login.expires_at_ms,
        user: UserSummary {
            id: login.user.user_id,
            name: login.user.display_name,
            role: login.user.role,
        },
        business: BusinessSummary {
            id: login.user.business_id,
            name: login.user.business_name,
        },
    }))
}

/// Imports a desktop snapshot into an empty server.
pub async fn link(
    State(state): State<AppState>,
    Json(request): Json<snapshot::LinkRequest>,
) -> Result<Json<snapshot::LinkResponse>, AppError> {
    let response = snapshot::link(&state.pool, request).await?;

    Ok(Json(response))
}

/// Appends desktop operations to the server log.
pub async fn push(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<sync::PushRequest>,
) -> Result<Json<sync::PushResponse>, AppError> {
    let principal = auth::sync_principal(&state.pool, &headers).await?;

    if let auth::SyncPrincipal::Device(device) = &principal {
        for operation in &request.operations {
            if operation.device_id.to_string() != device.device_id {
                return Err(AppError::Forbidden);
            }
        }
    }

    let response = sync::push(&state.pool, request.operations).await?;

    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub struct PullQuery {
    #[serde(default)]
    pub since: Option<i64>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Returns operations after the client's cursor.
pub async fn pull(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PullQuery>,
) -> Result<Json<sync::PullResponse>, AppError> {
    let _principal = auth::sync_principal(&state.pool, &headers).await?;

    let response = sync::pull(
        &state.pool,
        query.since.unwrap_or(0),
        query.limit.unwrap_or(sync::DEFAULT_PULL_LIMIT),
    )
    .await?;

    Ok(Json(response))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use http_body_util::BodyExt;
    use nexo_core::{DeviceId, Operation, OperationKind, Uuid};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    use super::*;
    use crate::router;
    use crate::test_support;

    async fn json_body(response: axum::response::Response) -> Value {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    async fn api_login(state: &AppState) -> String {
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"user":"Admin","pin":"1234"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let value = json_body(response).await;
        value["token"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn health_reports_the_database() {
        let state = test_support::state().await;

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let value = json_body(response).await;
        assert_eq!(value["status"], "ok");
        assert_eq!(value["database"], "ok");
    }

    #[tokio::test]
    async fn login_rejects_bad_credentials() {
        let state = test_support::state().await;

        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"user":"Admin","pin":"0000"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn login_is_throttled_after_repeated_failures() {
        let state = test_support::state().await;

        for _ in 0..auth::LOGIN_MAX_FAILURES {
            let response = router(state.clone())
                .oneshot(
                    Request::builder()
                        .method("POST")
                        .uri("/api/login")
                        .header(header::CONTENT_TYPE, "application/json")
                        .body(Body::from(r#"{"user":"Admin","pin":"0000"}"#))
                        .unwrap(),
                )
                .await
                .unwrap();

            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        }

        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/login")
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(r#"{"user":"Admin","pin":"1234"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers().get(header::RETRY_AFTER).unwrap(), "900");
        let value = json_body(response).await;
        assert_eq!(value["error"], "too many attempts");
    }

    #[tokio::test]
    async fn push_and_pull_round_trip_with_a_bearer_token() {
        let state = test_support::state().await;
        let token = api_login(&state).await;
        let operation = Operation::new(
            DeviceId::new(),
            None,
            OperationKind::OrderOpened,
            Uuid::now_v7(),
            json!({ "table": "M1" }),
            1_700_000_000_000,
        );

        let push_body = json!({ "operations": [operation] }).to_string();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sync/push")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::from(push_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let pushed = json_body(response).await;
        assert_eq!(pushed["accepted"], 1);
        assert_eq!(pushed["duplicates"], 0);
        assert_eq!(pushed["last_seq"], 1);

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/sync/pull?since=0")
                    .header(header::AUTHORIZATION, format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let pulled = json_body(response).await;
        assert_eq!(pulled["operations"].as_array().unwrap().len(), 1);
        assert_eq!(pulled["operations"][0]["seq"], 1);
        assert_eq!(
            pulled["operations"][0]["operation"]["id"],
            operation.id.to_string()
        );
        assert_eq!(pulled["next"], 1);
        assert_eq!(pulled["has_more"], false);
    }

    #[tokio::test]
    async fn sync_requires_an_api_token() {
        let state = test_support::state().await;

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/sync/pull?since=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn linked_device_can_push_and_pull_with_its_token() {
        let state = test_support::state().await;
        let snapshot = snapshot::sample_snapshot();
        let item_id = snapshot.items[0].id.parse::<Uuid>().unwrap();

        let link_response = snapshot::link(
            &state.pool,
            snapshot::LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot,
            },
        )
        .await
        .unwrap();

        let operation = Operation::new(
            link_response.device_id.parse::<DeviceId>().unwrap(),
            Some(link_response.branch_id.parse().unwrap()),
            OperationKind::MenuItemAvailabilityChanged,
            item_id,
            json!({ "is_available": false }),
            1_700_000_000_000,
        );

        let push_body = json!({ "operations": [operation] }).to_string();
        let response = router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sync/push")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", link_response.device_token),
                    )
                    .body(Body::from(push_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let pushed = json_body(response).await;
        assert_eq!(pushed["accepted"], 1);

        let available: bool =
            sqlx::query_scalar("SELECT is_available FROM menu_item WHERE id = ?1")
                .bind(item_id.to_string())
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert!(!available);

        let response = router(state)
            .oneshot(
                Request::builder()
                    .uri("/api/sync/pull?since=0")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", link_response.device_token),
                    )
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let pulled = json_body(response).await;
        assert_eq!(pulled["operations"].as_array().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn a_device_cannot_push_operations_for_another_device() {
        let state = test_support::state().await;
        let link_response = snapshot::link(
            &state.pool,
            snapshot::LinkRequest {
                server_user: "Admin".to_owned(),
                server_pin: "1234".to_owned(),
                snapshot: snapshot::sample_snapshot(),
            },
        )
        .await
        .unwrap();

        let operation = Operation::new(
            DeviceId::new(),
            None,
            OperationKind::MenuItemAvailabilityChanged,
            Uuid::now_v7(),
            json!({ "is_available": false }),
            1_700_000_000_000,
        );

        let push_body = json!({ "operations": [operation] }).to_string();
        let response = router(state)
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/sync/push")
                    .header(header::CONTENT_TYPE, "application/json")
                    .header(
                        header::AUTHORIZATION,
                        format!("Bearer {}", link_response.device_token),
                    )
                    .body(Body::from(push_body))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
