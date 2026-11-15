use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::http::HeaderMap;
use axum::http::header::AUTHORIZATION;
use axum_extra::extract::cookie::{Cookie, CookieJar, SameSite};
use nexo_core::{BranchId, BusinessId, DeviceId, UserId, Uuid};
use rand_core::RngCore;
use sha2::{Digest, Sha256};
use sqlx::{Row, SqlitePool};

use crate::config::Config;
use crate::error::AppError;
use crate::time::now_ms;

/// Name of the web session cookie.
pub const SESSION_COOKIE: &str = "nexo_session";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Web,
    Api,
}

impl SessionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Web => "web",
            Self::Api => "api",
        }
    }
}

/// Authenticated user and session metadata.
#[derive(Debug, Clone)]
pub struct SessionUser {
    pub session_id: String,
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub business_id: String,
    pub business_name: String,
    pub csrf_token: String,
    pub device_id: Option<String>,
}

/// Result of a successful PIN login.
#[derive(Debug, Clone)]
pub struct LoginResult {
    pub token: String,
    pub expires_at_ms: i64,
    pub user: SessionUser,
}

/// Creates the first business, branch, server device, and admin user.
///
/// The seed only runs when there are no users and `NEXO_ADMIN_PIN` is set.
pub async fn seed_admin(pool: &SqlitePool, config: &Config) -> Result<(), AppError> {
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM app_user")
        .fetch_one(pool)
        .await?;

    if user_count > 0 {
        return Ok(());
    }

    let Some(pin) = config.admin_pin.as_deref() else {
        tracing::warn!(
            "no users exist and NEXO_ADMIN_PIN is not set; web and API login are disabled"
        );
        return Ok(());
    };

    validate_pin(pin)?;

    let pin_hash = hash_pin(pin)?;
    let now = now_ms();
    let business_id = BusinessId::new();
    let branch_id = BranchId::new();
    let device_id = DeviceId::new();
    let user_id = UserId::new();

    let mut transaction = pool.begin().await?;

    sqlx::query(
        "INSERT INTO business (
            id, name, base_currency, locale, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, 'USD', 'es', ?3, ?3)",
    )
    .bind(business_id.to_string())
    .bind(&config.business_name)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO branch (
            id, business_id, name, is_default, created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, 'Principal', 1, ?3, ?3)",
    )
    .bind(branch_id.to_string())
    .bind(business_id.to_string())
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO device (
            id, business_id, branch_id, name, kind, is_master, registered_at_ms
         ) VALUES (?1, ?2, ?3, 'Servidor', 'server', 1, ?4)",
    )
    .bind(device_id.to_string())
    .bind(business_id.to_string())
    .bind(branch_id.to_string())
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    sqlx::query(
        "INSERT INTO app_user (
            id, business_id, display_name, role, pin_hash, is_active,
            created_at_ms, updated_at_ms
         ) VALUES (?1, ?2, 'Admin', 'admin', ?3, 1, ?4, ?4)",
    )
    .bind(user_id.to_string())
    .bind(business_id.to_string())
    .bind(pin_hash)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;

    tracing::info!(
        business = %config.business_name,
        "seeded the first admin user"
    );

    Ok(())
}

/// Validates a user PIN and creates a session.
///
/// Returns `None` when the user does not exist, is inactive, or the PIN is
/// wrong. The caller must not reveal which case happened.
pub async fn login(
    pool: &SqlitePool,
    user_name: &str,
    pin: &str,
    kind: SessionKind,
    device_id: Option<&str>,
    session_hours: i64,
) -> Result<Option<LoginResult>, AppError> {
    if let Some(device_id) = device_id {
        device_id
            .parse::<DeviceId>()
            .map_err(|_| AppError::Validation("invalid device id".to_owned()))?;
    }

    let row = sqlx::query(
        "SELECT
            u.id AS user_id,
            u.display_name,
            u.role,
            u.pin_hash,
            b.id AS business_id,
            b.name AS business_name
         FROM app_user u
         JOIN business b ON b.id = u.business_id
         WHERE u.display_name = ?1 COLLATE NOCASE AND u.is_active = 1
         LIMIT 1",
    )
    .bind(user_name.trim())
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let pin_hash: Option<String> = row.try_get("pin_hash")?;
    let Some(pin_hash) = pin_hash else {
        return Ok(None);
    };

    if !verify_pin(&pin_hash, pin)? {
        return Ok(None);
    }

    let token = random_hex(32);
    let token_hash = sha256_hex(&token);
    let csrf_token = random_hex(32);
    let session_id = Uuid::now_v7().to_string();
    let now = now_ms();
    let expires_at_ms = now + session_hours * 60 * 60 * 1000;

    sqlx::query(
        "INSERT INTO auth_session (
            id, business_id, user_id, device_id, kind, token_hash, csrf_token,
            created_at_ms, expires_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
    )
    .bind(&session_id)
    .bind(row.try_get::<String, _>("business_id")?)
    .bind(row.try_get::<String, _>("user_id")?)
    .bind(device_id)
    .bind(kind.as_str())
    .bind(token_hash)
    .bind(&csrf_token)
    .bind(now)
    .bind(expires_at_ms)
    .execute(pool)
    .await?;

    Ok(Some(LoginResult {
        token,
        expires_at_ms,
        user: SessionUser {
            session_id,
            user_id: row.try_get("user_id")?,
            display_name: row.try_get("display_name")?,
            role: row.try_get("role")?,
            business_id: row.try_get("business_id")?,
            business_name: row.try_get("business_name")?,
            csrf_token,
            device_id: device_id.map(str::to_owned),
        },
    }))
}

/// Loads a web session from the session cookie.
pub async fn web_session(
    pool: &SqlitePool,
    jar: &CookieJar,
) -> Result<Option<SessionUser>, AppError> {
    let Some(cookie) = jar.get(SESSION_COOKIE) else {
        return Ok(None);
    };

    lookup_session(pool, cookie.value(), SessionKind::Web).await
}

/// Loads an API session from the bearer token.
pub async fn api_session(pool: &SqlitePool, headers: &HeaderMap) -> Result<SessionUser, AppError> {
    let token = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(AppError::Unauthorized)?;

    lookup_session(pool, token, SessionKind::Api)
        .await?
        .ok_or(AppError::Unauthorized)
}

/// Revokes a session. Logout is idempotent.
pub async fn revoke_session(pool: &SqlitePool, session_id: &str) -> Result<(), AppError> {
    sqlx::query(
        "UPDATE auth_session
         SET revoked_at_ms = ?1
         WHERE id = ?2 AND revoked_at_ms IS NULL",
    )
    .bind(now_ms())
    .bind(session_id)
    .execute(pool)
    .await?;

    Ok(())
}

/// Builds the web session cookie.
pub fn session_cookie(token: String) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, token))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .build()
}

/// Builds a removal cookie for logout.
pub fn removal_cookie() -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, ""))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .build()
}

/// Returns true when the submitted CSRF token matches the session.
pub fn csrf_matches(session: &SessionUser, submitted: &str) -> bool {
    !session.csrf_token.is_empty() && session.csrf_token == submitted
}

/// Maps a role to its Spanish label for the web app.
pub fn role_label(role: &str) -> &'static str {
    match role {
        "admin" => "Administrador",
        "cashier" => "Cajero",
        "waiter" => "Mesonero",
        "kitchen" => "Cocina",
        _ => "Usuario",
    }
}

/// PINs are 4 to 8 digits.
pub fn validate_pin(pin: &str) -> Result<(), AppError> {
    if pin.len() < 4 || pin.len() > 8 || !pin.chars().all(|character| character.is_ascii_digit()) {
        return Err(AppError::Config(
            "NEXO_ADMIN_PIN must have 4 to 8 digits".to_owned(),
        ));
    }

    Ok(())
}

async fn lookup_session(
    pool: &SqlitePool,
    token: &str,
    kind: SessionKind,
) -> Result<Option<SessionUser>, AppError> {
    let token_hash = sha256_hex(token);

    let row = sqlx::query(
        "SELECT
            s.id AS session_id,
            s.csrf_token,
            s.device_id,
            u.id AS user_id,
            u.display_name,
            u.role,
            b.id AS business_id,
            b.name AS business_name
         FROM auth_session s
         JOIN app_user u ON u.id = s.user_id
         JOIN business b ON b.id = s.business_id
         WHERE s.token_hash = ?1
           AND s.kind = ?2
           AND s.revoked_at_ms IS NULL
           AND s.expires_at_ms > ?3
         LIMIT 1",
    )
    .bind(token_hash)
    .bind(kind.as_str())
    .bind(now_ms())
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    Ok(Some(SessionUser {
        session_id: row.try_get("session_id")?,
        user_id: row.try_get("user_id")?,
        display_name: row.try_get("display_name")?,
        role: row.try_get("role")?,
        business_id: row.try_get("business_id")?,
        business_name: row.try_get("business_name")?,
        csrf_token: row.try_get("csrf_token")?,
        device_id: row.try_get("device_id")?,
    }))
}

fn hash_pin(pin: &str) -> Result<String, AppError> {
    let salt = SaltString::generate(&mut OsRng);

    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|error| AppError::Internal(format!("could not hash PIN: {error}")))
}

fn verify_pin(hash: &str, pin: &str) -> Result<bool, AppError> {
    let parsed = PasswordHash::new(hash)
        .map_err(|error| AppError::CorruptData(format!("invalid PIN hash: {error}")))?;

    Ok(Argon2::default()
        .verify_password(pin.as_bytes(), &parsed)
        .is_ok())
}

fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    OsRng.fill_bytes(&mut buffer);
    hex::encode(buffer)
}

fn sha256_hex(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    async fn pool_with_admin() -> SqlitePool {
        let pool = db::connect_in_memory().await.unwrap();
        db::migrate(&pool).await.unwrap();

        let config = Config {
            database_url: "sqlite::memory:".to_owned(),
            addr: "127.0.0.1:0".to_owned(),
            admin_pin: Some("1234".to_owned()),
            business_name: "Café Central".to_owned(),
            session_hours: 12,
        };

        seed_admin(&pool, &config).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn seed_creates_one_admin_and_is_idempotent() {
        let pool = pool_with_admin().await;

        let users: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM app_user")
            .fetch_one(&pool)
            .await
            .unwrap();
        let businesses: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM business")
            .fetch_one(&pool)
            .await
            .unwrap();

        assert_eq!(users, 1);
        assert_eq!(businesses, 1);

        let config = Config {
            database_url: "sqlite::memory:".to_owned(),
            addr: "127.0.0.1:0".to_owned(),
            admin_pin: Some("9999".to_owned()),
            business_name: "Other".to_owned(),
            session_hours: 12,
        };
        seed_admin(&pool, &config).await.unwrap();

        let businesses: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM business")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(businesses, 1);
    }

    #[tokio::test]
    async fn login_accepts_the_right_pin_and_rejects_the_wrong_one() {
        let pool = pool_with_admin().await;

        let session = login(&pool, "admin", "1234", SessionKind::Web, None, 12)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(session.user.display_name, "Admin");
        assert_eq!(session.user.role, "admin");
        assert_eq!(session.user.business_name, "Café Central");
        assert!(!session.token.is_empty());

        let wrong = login(&pool, "Admin", "0000", SessionKind::Web, None, 12)
            .await
            .unwrap();
        assert!(wrong.is_none());

        let missing = login(&pool, "Nobody", "1234", SessionKind::Web, None, 12)
            .await
            .unwrap();
        assert!(missing.is_none());
    }

    #[tokio::test]
    async fn sessions_are_kind_scoped_and_revocable() {
        let pool = pool_with_admin().await;
        let session = login(&pool, "Admin", "1234", SessionKind::Web, None, 12)
            .await
            .unwrap()
            .unwrap();

        let stored_hash: String = sqlx::query_scalar("SELECT token_hash FROM auth_session")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_ne!(stored_hash, session.token);

        assert!(
            lookup_session(&pool, &session.token, SessionKind::Web)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            lookup_session(&pool, &session.token, SessionKind::Api)
                .await
                .unwrap()
                .is_none()
        );

        revoke_session(&pool, &session.user.session_id)
            .await
            .unwrap();

        assert!(
            lookup_session(&pool, &session.token, SessionKind::Web)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn pin_validation_requires_four_to_eight_digits() {
        assert!(validate_pin("1234").is_ok());
        assert!(validate_pin("12345678").is_ok());
        assert!(validate_pin("123").is_err());
        assert!(validate_pin("123456789").is_err());
        assert!(validate_pin("12a4").is_err());
    }
}
