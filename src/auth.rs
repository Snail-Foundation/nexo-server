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

/// Name of the cookie that identifies one browser as a cash register.
pub const WEB_DEVICE_COOKIE: &str = "nexo_web_device";

/// Failed PIN attempts allowed before the first lock.
pub const LOGIN_MAX_FAILURES: i64 = 5;

/// Duration of the failure-counting window. It is long enough for the short
/// first locks to expire without resetting the escalation.
pub const LOGIN_WINDOW_MS: i64 = 60 * 60 * 1000;

/// First lock: five failures.
pub const LOGIN_FIRST_LOCK_MS: i64 = 60 * 1000;

/// Second lock: ten failures.
pub const LOGIN_SECOND_LOCK_MS: i64 = 5 * 60 * 1000;

/// Third lock: fifteen failures.
pub const LOGIN_THIRD_LOCK_MS: i64 = 15 * 60 * 1000;

/// Longest lock: twenty or more failures.
pub const LOGIN_FOURTH_LOCK_MS: i64 = 60 * 60 * 1000;

/// Returns the progressive lock duration for a failure count.
fn login_lock_ms(failure_count: i64) -> i64 {
    match failure_count / LOGIN_MAX_FAILURES {
        0 | 1 => LOGIN_FIRST_LOCK_MS,
        2 => LOGIN_SECOND_LOCK_MS,
        3 => LOGIN_THIRD_LOCK_MS,
        _ => LOGIN_FOURTH_LOCK_MS,
    }
}

/// Throttle rows older than this are pruned on the next failed attempt.
const LOGIN_THROTTLE_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;

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

/// Authenticated user data without a session. Used by the linking endpoint.
#[derive(Debug, Clone)]
pub struct AuthenticatedUser {
    pub user_id: String,
    pub display_name: String,
    pub role: String,
    pub business_id: String,
    pub business_name: String,
}

/// A desktop device authenticated by its device token.
#[derive(Debug, Clone)]
pub struct DeviceSession {
    pub device_id: String,
    pub business_id: String,
    pub branch_id: Option<String>,
}

/// Caller allowed to push or pull operations.
#[derive(Debug, Clone)]
pub enum SyncPrincipal {
    User(SessionUser),
    Device(DeviceSession),
}

impl SyncPrincipal {
    /// Returns the device ID that owns the session, if any.
    pub fn device_id(&self) -> Option<&str> {
        match self {
            Self::User(user) => user.device_id.as_deref(),
            Self::Device(device) => Some(&device.device_id),
        }
    }

    /// Returns the business of the caller.
    pub fn business_id(&self) -> &str {
        match self {
            Self::User(user) => &user.business_id,
            Self::Device(device) => &device.business_id,
        }
    }
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

    let Some(user) = authenticate_throttled(pool, user_name, pin).await? else {
        return Ok(None);
    };

    let token = random_token();
    let token_hash = hash_token(&token);
    let csrf_token = random_token();
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
    .bind(&user.business_id)
    .bind(&user.user_id)
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
            user_id: user.user_id,
            display_name: user.display_name,
            role: user.role,
            business_id: user.business_id,
            business_name: user.business_name,
            csrf_token,
            device_id: device_id.map(str::to_owned),
        },
    }))
}

/// Verifies a user name and PIN without creating a session.
pub async fn authenticate(
    pool: &SqlitePool,
    user_name: &str,
    pin: &str,
) -> Result<Option<AuthenticatedUser>, AppError> {
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

    Ok(Some(AuthenticatedUser {
        user_id: row.try_get("user_id")?,
        display_name: row.try_get("display_name")?,
        role: row.try_get("role")?,
        business_id: row.try_get("business_id")?,
        business_name: row.try_get("business_name")?,
    }))
}

/// Verifies credentials while enforcing the persistent login throttle.
///
/// This is the entry point used by web login, API login, and snapshot linking,
/// so all PIN verification paths share the same failure counter.
pub async fn authenticate_throttled(
    pool: &SqlitePool,
    user_name: &str,
    pin: &str,
) -> Result<Option<AuthenticatedUser>, AppError> {
    let key_hash = throttle_key(user_name);
    let now = now_ms();

    ensure_login_allowed_at(pool, &key_hash, now).await?;

    let user = authenticate(pool, user_name, pin).await?;

    if user.is_some() {
        clear_login_failures(pool, &key_hash).await?;
    } else {
        record_login_failure_at(pool, &key_hash, now).await?;
    }

    Ok(user)
}

/// Hashes a normalized user name for the throttle table.
pub(crate) fn throttle_key(user_name: &str) -> String {
    sha256_hex(&user_name.trim().to_lowercase())
}

async fn ensure_login_allowed_at(
    pool: &SqlitePool,
    key_hash: &str,
    now: i64,
) -> Result<(), AppError> {
    let locked_until_ms: Option<Option<i64>> = sqlx::query_scalar(
        "SELECT locked_until_ms
         FROM login_throttle
         WHERE key_hash = ?1",
    )
    .bind(key_hash)
    .fetch_optional(pool)
    .await?;

    if let Some(locked_until) = locked_until_ms.flatten().filter(|locked| *locked > now) {
        let retry_after_seconds = ((locked_until - now + 999) / 1_000).max(1);

        return Err(AppError::TooManyAttempts {
            retry_after_seconds,
        });
    }

    Ok(())
}

async fn record_login_failure_at(
    pool: &SqlitePool,
    key_hash: &str,
    now: i64,
) -> Result<(), AppError> {
    let mut transaction = pool.begin().await?;

    let existing = sqlx::query(
        "SELECT failure_count, window_started_at_ms, locked_until_ms
         FROM login_throttle
         WHERE key_hash = ?1",
    )
    .bind(key_hash)
    .fetch_optional(&mut *transaction)
    .await?;

    let (failure_count, window_started_at_ms, locked_until_ms) = match existing {
        Some(row) => (
            row.try_get::<i64, _>("failure_count")?,
            row.try_get::<i64, _>("window_started_at_ms")?,
            row.try_get::<Option<i64>, _>("locked_until_ms")?,
        ),
        None => (0, now, None),
    };

    // A concurrent request may have locked the account already. Do not extend
    // the lock, so a locked user cannot be locked out indefinitely.
    if locked_until_ms.is_some_and(|locked| locked > now) {
        transaction.rollback().await?;
        return Ok(());
    }

    let window_expired = now.saturating_sub(window_started_at_ms) >= LOGIN_WINDOW_MS;
    let (failure_count, window_started_at_ms) = if window_expired {
        (1, now)
    } else {
        (failure_count + 1, window_started_at_ms)
    };
    let lock_due = failure_count >= LOGIN_MAX_FAILURES && failure_count % LOGIN_MAX_FAILURES == 0;
    let locked_until_ms = lock_due.then_some(now + login_lock_ms(failure_count));

    sqlx::query(
        "INSERT INTO login_throttle (
            key_hash, failure_count, window_started_at_ms, locked_until_ms,
            updated_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(key_hash) DO UPDATE SET
            failure_count = excluded.failure_count,
            window_started_at_ms = excluded.window_started_at_ms,
            locked_until_ms = excluded.locked_until_ms,
            updated_at_ms = excluded.updated_at_ms",
    )
    .bind(key_hash)
    .bind(failure_count)
    .bind(window_started_at_ms)
    .bind(locked_until_ms)
    .bind(now)
    .execute(&mut *transaction)
    .await?;

    sqlx::query("DELETE FROM login_throttle WHERE updated_at_ms < ?1")
        .bind(now - LOGIN_THROTTLE_RETENTION_MS)
        .execute(&mut *transaction)
        .await?;

    transaction.commit().await?;

    Ok(())
}

async fn clear_login_failures(pool: &SqlitePool, key_hash: &str) -> Result<(), AppError> {
    sqlx::query("DELETE FROM login_throttle WHERE key_hash = ?1")
        .bind(key_hash)
        .execute(pool)
        .await?;

    Ok(())
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
    let token = bearer_token(headers).ok_or(AppError::Unauthorized)?;

    lookup_session(pool, token, SessionKind::Api)
        .await?
        .ok_or(AppError::Unauthorized)
}

/// Loads the caller of a sync request: an API session or a linked device.
pub async fn sync_principal(
    pool: &SqlitePool,
    headers: &HeaderMap,
) -> Result<SyncPrincipal, AppError> {
    let token = bearer_token(headers).ok_or(AppError::Unauthorized)?;

    if let Some(user) = lookup_session(pool, token, SessionKind::Api).await? {
        return Ok(SyncPrincipal::User(user));
    }

    if let Some(device) = lookup_device(pool, token).await? {
        return Ok(SyncPrincipal::Device(device));
    }

    Err(AppError::Unauthorized)
}

/// Returns the default branch of a business.
pub async fn default_branch_id(
    pool: &SqlitePool,
    business_id: &str,
) -> Result<Option<String>, AppError> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM branch
         WHERE business_id = ?1
         ORDER BY is_default DESC, created_at_ms
         LIMIT 1",
    )
    .bind(business_id)
    .fetch_optional(pool)
    .await?)
}

/// Generates a random bearer token for a device.
pub fn random_token() -> String {
    random_hex(32)
}

/// Hashes a bearer token before storing it.
pub fn hash_token(token: &str) -> String {
    sha256_hex(token)
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

/// Returns the device that represents this browser as a cash register.
///
/// A browser keeps its device in a long-lived cookie so its cash session
/// survives page reloads and logins. If the cookie is missing or does not
/// belong to the business, a new web device is created.
pub async fn resolve_web_device(
    pool: &SqlitePool,
    jar: &CookieJar,
    session: &SessionUser,
) -> Result<(String, Option<Cookie<'static>>), AppError> {
    if let Some(cookie) = jar.get(WEB_DEVICE_COOKIE) {
        let device_id = cookie.value().to_owned();
        let valid: bool = sqlx::query_scalar(
            "SELECT EXISTS (
                SELECT 1 FROM device
                WHERE id = ?1 AND business_id = ?2 AND kind = 'web'
             )",
        )
        .bind(&device_id)
        .bind(&session.business_id)
        .fetch_one(pool)
        .await?;

        if valid {
            let now = now_ms();
            sqlx::query("UPDATE device SET last_seen_at_ms = ?2 WHERE id = ?1")
                .bind(&device_id)
                .bind(now)
                .execute(pool)
                .await?;
            sqlx::query("UPDATE auth_session SET device_id = ?2 WHERE id = ?1")
                .bind(&session.session_id)
                .bind(&device_id)
                .execute(pool)
                .await?;
            return Ok((device_id, None));
        }
    }

    let device_id = create_web_device(pool, &session.business_id, &session.session_id).await?;

    let cookie = Cookie::build((WEB_DEVICE_COOKIE, device_id.to_string()))
        .http_only(true)
        .same_site(SameSite::Strict)
        .path("/")
        .max_age(time::Duration::days(365))
        .build();

    Ok((device_id.to_string(), Some(cookie)))
}

/// Creates a web device for a browser and attaches it to the session.
///
/// The caller decides whether the device is also stored in a cookie. This is
/// used for new logins and for sessions created before web devices existed.
pub async fn create_web_device(
    pool: &SqlitePool,
    business_id: &str,
    session_id: &str,
) -> Result<DeviceId, AppError> {
    let device_id = DeviceId::new();
    let branch_id: Option<String> = sqlx::query_scalar(
        "SELECT id FROM branch
         WHERE business_id = ?1
         ORDER BY is_default DESC, created_at_ms
         LIMIT 1",
    )
    .bind(business_id)
    .fetch_optional(pool)
    .await?;
    let now = now_ms();

    sqlx::query(
        "INSERT INTO device (
            id, business_id, branch_id, name, kind, is_master, registered_at_ms,
            last_seen_at_ms
         ) VALUES (?1, ?2, ?3, 'Caja web', 'web', 0, ?4, ?4)",
    )
    .bind(device_id.to_string())
    .bind(business_id)
    .bind(branch_id)
    .bind(now)
    .execute(pool)
    .await?;

    sqlx::query("UPDATE auth_session SET device_id = ?2 WHERE id = ?1")
        .bind(session_id)
        .bind(device_id.to_string())
        .execute(pool)
        .await?;

    Ok(device_id)
}

/// Returns true when the submitted CSRF token matches the session.
///
/// The comparison scans the whole token instead of stopping at the first
/// differing byte.
pub fn csrf_matches(session: &SessionUser, submitted: &str) -> bool {
    !session.csrf_token.is_empty()
        && constant_time_eq(session.csrf_token.as_bytes(), submitted.as_bytes())
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut difference = 0u8;
    for (left, right) in left.iter().zip(right) {
        difference |= left ^ right;
    }

    difference == 0
}

/// Roles that can serve tables, takeout, and order actions.
pub fn can_serve(role: &str) -> bool {
    matches!(role, "admin" | "cashier" | "waiter")
}

/// Roles that can work the kitchen board.
pub fn can_kitchen(role: &str) -> bool {
    matches!(role, "admin" | "kitchen")
}

/// Roles that can manage cash and reports in the web app.
pub fn can_manage(role: &str) -> bool {
    matches!(role, "admin" | "cashier")
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

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
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
           AND u.is_active = 1
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

async fn lookup_device(pool: &SqlitePool, token: &str) -> Result<Option<DeviceSession>, AppError> {
    let token_hash = sha256_hex(token);

    let row = sqlx::query(
        "SELECT id, business_id, branch_id
         FROM device
         WHERE token_hash = ?1 AND kind = 'desktop'
         LIMIT 1",
    )
    .bind(token_hash)
    .fetch_optional(pool)
    .await?;

    let Some(row) = row else {
        return Ok(None);
    };

    let device_id: String = row.try_get("id")?;

    sqlx::query("UPDATE device SET last_seen_at_ms = ?2 WHERE id = ?1")
        .bind(&device_id)
        .bind(now_ms())
        .execute(pool)
        .await?;

    Ok(Some(DeviceSession {
        device_id,
        business_id: row.try_get("business_id")?,
        branch_id: row.try_get("branch_id")?,
    }))
}

pub fn hash_pin(pin: &str) -> Result<String, AppError> {
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

    #[tokio::test]
    async fn inactive_users_cannot_use_existing_sessions() {
        let pool = pool_with_admin().await;
        let web = login(&pool, "Admin", "1234", SessionKind::Web, None, 12)
            .await
            .unwrap()
            .unwrap();
        let api = login(&pool, "Admin", "1234", SessionKind::Api, None, 12)
            .await
            .unwrap()
            .unwrap();

        sqlx::query("UPDATE app_user SET is_active = 0")
            .execute(&pool)
            .await
            .unwrap();

        let jar = CookieJar::new().add(Cookie::new(SESSION_COOKIE, web.token.clone()));
        assert!(web_session(&pool, &jar).await.unwrap().is_none());

        let mut headers = HeaderMap::new();
        headers.insert(
            AUTHORIZATION,
            format!("Bearer {}", api.token).parse().unwrap(),
        );
        assert!(matches!(
            api_session(&pool, &headers).await,
            Err(AppError::Unauthorized)
        ));

        assert!(
            login(&pool, "Admin", "1234", SessionKind::Web, None, 12)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn failed_logins_lock_the_account_until_the_first_lock_expires() {
        let pool = pool_with_admin().await;
        let key = throttle_key("Admin");
        let start = now_ms();

        for _ in 0..LOGIN_MAX_FAILURES {
            record_login_failure_at(&pool, &key, start).await.unwrap();
        }

        assert!(matches!(
            ensure_login_allowed_at(&pool, &key, start + 1).await,
            Err(AppError::TooManyAttempts {
                retry_after_seconds: 60
            })
        ));

        // The correct PIN is rejected too while the account is locked.
        assert!(matches!(
            authenticate_throttled(&pool, "Admin", "1234").await,
            Err(AppError::TooManyAttempts { .. })
        ));

        assert!(
            ensure_login_allowed_at(&pool, &key, start + LOGIN_FIRST_LOCK_MS + 1)
                .await
                .is_ok()
        );
    }

    #[tokio::test]
    async fn retry_after_reports_the_remaining_lock() {
        let pool = pool_with_admin().await;
        let key = throttle_key("Admin");
        let start = now_ms();

        for _ in 0..LOGIN_MAX_FAILURES {
            record_login_failure_at(&pool, &key, start).await.unwrap();
        }

        let Err(AppError::TooManyAttempts {
            retry_after_seconds,
        }) = ensure_login_allowed_at(&pool, &key, start + 30_000).await
        else {
            panic!("expected a lock");
        };

        assert_eq!(retry_after_seconds, 30);
    }

    #[tokio::test]
    async fn repeated_failures_escalate_the_lock_duration() {
        let pool = pool_with_admin().await;
        let key = throttle_key("Admin");
        let mut now = now_ms();
        let mut expected = [
            LOGIN_FIRST_LOCK_MS,
            LOGIN_SECOND_LOCK_MS,
            LOGIN_THIRD_LOCK_MS,
            LOGIN_FOURTH_LOCK_MS,
        ];

        for lock_ms in expected.iter_mut() {
            for _ in 0..LOGIN_MAX_FAILURES {
                record_login_failure_at(&pool, &key, now).await.unwrap();
            }

            let locked: Option<i64> = sqlx::query_scalar(
                "SELECT locked_until_ms FROM login_throttle WHERE key_hash = ?1",
            )
            .bind(&key)
            .fetch_one(&pool)
            .await
            .unwrap();
            assert_eq!(locked, Some(now + *lock_ms));

            now += *lock_ms + 1;
        }
    }

    #[tokio::test]
    async fn the_failure_window_resets_after_it_expires() {
        let pool = pool_with_admin().await;
        let key = throttle_key("Admin");
        let start = now_ms();

        for _ in 0..(LOGIN_MAX_FAILURES - 1) {
            record_login_failure_at(&pool, &key, start).await.unwrap();
        }

        record_login_failure_at(&pool, &key, start + LOGIN_WINDOW_MS + 1)
            .await
            .unwrap();

        let count: i64 =
            sqlx::query_scalar("SELECT failure_count FROM login_throttle WHERE key_hash = ?1")
                .bind(&key)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(count, 1);

        let locked: Option<i64> =
            sqlx::query_scalar("SELECT locked_until_ms FROM login_throttle WHERE key_hash = ?1")
                .bind(&key)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(locked.is_none());
    }

    #[tokio::test]
    async fn a_successful_login_clears_failed_attempts() {
        let pool = pool_with_admin().await;

        assert!(
            login(&pool, "Admin", "0000", SessionKind::Web, None, 12)
                .await
                .unwrap()
                .is_none()
        );

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM login_throttle")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 1);

        login(&pool, "Admin", "1234", SessionKind::Web, None, 12)
            .await
            .unwrap()
            .unwrap();

        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM login_throttle")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[test]
    fn csrf_tokens_are_compared_exactly() {
        let session = SessionUser {
            session_id: "session".to_owned(),
            user_id: "user".to_owned(),
            display_name: "Admin".to_owned(),
            role: "admin".to_owned(),
            business_id: "business".to_owned(),
            business_name: "Café Central".to_owned(),
            csrf_token: "abc123".to_owned(),
            device_id: None,
        };

        assert!(csrf_matches(&session, "abc123"));
        assert!(!csrf_matches(&session, "abc124"));
        assert!(!csrf_matches(&session, "abc12"));
        assert!(!csrf_matches(&session, ""));
        assert!(!csrf_matches(&session, "abc1234"));

        let empty = SessionUser {
            csrf_token: String::new(),
            ..session
        };
        assert!(!csrf_matches(&empty, ""));
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
