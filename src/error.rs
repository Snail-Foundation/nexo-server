use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use thiserror::Error;

/// Server errors. Public responses never expose internal details.
#[derive(Debug, Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("template error: {0}")]
    Template(#[from] askama::Error),

    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("invalid configuration: {0}")]
    Config(String),

    #[error("internal error: {0}")]
    Internal(String),

    #[error("unauthorized")]
    Unauthorized,

    #[error("forbidden")]
    Forbidden,

    #[error("not found")]
    NotFound,

    #[error("validation failed: {0}")]
    Validation(String),

    #[error("corrupt data: {0}")]
    CorruptData(String),
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, message) = match &self {
            Self::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized".to_owned()),
            Self::Forbidden => (StatusCode::FORBIDDEN, "forbidden".to_owned()),
            Self::NotFound => (StatusCode::NOT_FOUND, "not found".to_owned()),
            Self::Validation(message) => (StatusCode::UNPROCESSABLE_ENTITY, message.clone()),
            Self::Config(message) => {
                tracing::error!(error = %message, "invalid configuration");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "invalid configuration".to_owned(),
                )
            }
            other => {
                tracing::error!(error = %other, "request failed");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "internal server error".to_owned(),
                )
            }
        };

        (status, Json(json!({ "error": message }))).into_response()
    }
}
