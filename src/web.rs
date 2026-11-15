use askama::Template;
use axum::extract::{Form, State};
use axum::http::{StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum_extra::extract::CookieJar;
use serde::Deserialize;

use crate::auth::{self, SessionKind};
use crate::error::AppError;
use crate::state::AppState;
use crate::time::{format_local_time, now_ms};

const STYLESHEET: &str = include_str!("../static/app.css");
const HTMX: &str = include_str!("../static/htmx.min.js");

#[derive(Template)]
#[template(path = "login.html")]
struct LoginTemplate {
    error: String,
}

#[derive(Template)]
#[template(path = "app.html")]
struct AppTemplate {
    business_name: String,
    user_name: String,
    role_label: String,
    csrf_token: String,
    version: &'static str,
}

#[derive(Template)]
#[template(path = "status.html")]
struct StatusTemplate {
    server_time: String,
    user_name: String,
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

/// Sends the browser to the app or the login page.
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

/// Validates the PIN, creates a web session, and redirects to the app.
pub async fn login_submit(
    State(state): State<AppState>,
    jar: CookieJar,
    Form(form): Form<LoginForm>,
) -> Result<Response, AppError> {
    let Some(login) = auth::login(
        &state.pool,
        &form.user,
        &form.pin,
        SessionKind::Web,
        None,
        state.config.session_hours,
    )
    .await?
    else {
        let html = LoginTemplate {
            error: "Usuario o PIN incorrecto.".to_owned(),
        }
        .render()?;

        return Ok((StatusCode::UNAUTHORIZED, Html(html)).into_response());
    };

    let cookie = auth::session_cookie(login.token);

    Ok((jar.add(cookie), Redirect::to("/app")).into_response())
}

/// Renders the waiter/kitchen web app shell.
pub async fn app_page(State(state): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    let Some(session) = auth::web_session(&state.pool, &jar).await? else {
        return Ok(Redirect::to("/login").into_response());
    };

    let html = AppTemplate {
        business_name: session.business_name,
        user_name: session.display_name.clone(),
        role_label: auth::role_label(&session.role).to_owned(),
        csrf_token: session.csrf_token,
        version: env!("CARGO_PKG_VERSION"),
    }
    .render()?;

    Ok(Html(html).into_response())
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
        let response = router(state.clone())
            .oneshot(form_request("/login", "user=Admin&pin=1234"))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);

        response
            .headers()
            .get(header::SET_COOKIE)
            .unwrap()
            .to_str()
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .to_owned()
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

        let response = router(state)
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers().get(header::LOCATION).unwrap(), "/login");
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
}
