use std::env;

use crate::auth::validate_pin;
use crate::error::AppError;

pub const DEFAULT_DATABASE_URL: &str = "sqlite://nexo-server.db?mode=rwc";
pub const DEFAULT_ADDR: &str = "0.0.0.0:8080";
pub const DEFAULT_BUSINESS_NAME: &str = "Nexo Restaurantes";
pub const DEFAULT_SESSION_HOURS: i64 = 12;

/// Runtime configuration read from environment variables.
#[derive(Debug, Clone)]
pub struct Config {
    pub database_url: String,
    pub addr: String,
    pub admin_pin: Option<String>,
    pub business_name: String,
    pub session_hours: i64,
}

impl Config {
    pub fn from_env() -> Result<Self, AppError> {
        let admin_pin = env::var("NEXO_ADMIN_PIN")
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty());

        if let Some(pin) = &admin_pin {
            validate_pin(pin)?;
        }

        let session_hours = match env::var("NEXO_SESSION_HOURS") {
            Ok(value) if !value.trim().is_empty() => value
                .trim()
                .parse::<i64>()
                .map_err(|_| AppError::Config("NEXO_SESSION_HOURS must be a number".to_owned()))?,
            _ => DEFAULT_SESSION_HOURS,
        };

        if !(1..=168).contains(&session_hours) {
            return Err(AppError::Config(
                "NEXO_SESSION_HOURS must be between 1 and 168".to_owned(),
            ));
        }

        Ok(Self {
            database_url: env::var("NEXO_SERVER_DB")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_DATABASE_URL.to_owned()),
            addr: env::var("NEXO_SERVER_ADDR")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .unwrap_or_else(|| DEFAULT_ADDR.to_owned()),
            admin_pin,
            business_name: env::var("NEXO_BUSINESS_NAME")
                .ok()
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| DEFAULT_BUSINESS_NAME.to_owned()),
            session_hours,
        })
    }
}
