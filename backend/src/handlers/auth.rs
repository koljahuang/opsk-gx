use axum::response::{IntoResponse, Response};
use axum::{
    Json,
    extract::{Path, State},
    http::header::SET_COOKIE,
};
use uuid::Uuid;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::user::{ChangePasswordRequest, LoginRequest, LoginResponse, UserInfo};
use crate::services::{auth_common, refresh_token};

/// POST /api/auth/login
pub async fn login(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<std::net::SocketAddr>,
    headers: axum::http::HeaderMap,
    Json(req): Json<LoginRequest>,
) -> AppResult<Response> {
    // Find user by username
    let user =
        sqlx::query_as::<_, crate::models::user::User>("SELECT * FROM users WHERE username = $1 AND is_active = true")
            .bind(&req.username)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(|| AppError::Unauthorized("Invalid credentials".to_string()))?;

    // Verify password (use spawn_blocking for bcrypt)
    let password = req.password.clone();
    let hash = user
        .password_hash
        .clone()
        .ok_or_else(|| AppError::Unauthorized("This account uses OAuth login".to_string()))?;
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(password, &hash))
        .await
        .map_err(|_| AppError::Internal("Password verification failed".to_string()))?
        .map_err(|_| AppError::Unauthorized("Invalid credentials".to_string()))?;

    if !valid {
        return Err(AppError::Unauthorized("Invalid credentials".to_string()));
    }

    // Update last login
    sqlx::query("UPDATE users SET last_login_at = NOW() WHERE id = $1")
        .bind(user.id)
        .execute(&state.pool)
        .await?;

    // Generate access token (configurable expiry)
    let token = auth_common::create_access_token(&state.config, &user)?;

    // Generate refresh token
    let ip = addr.ip().to_string();
    let ua = headers
        .get(http::header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let (refresh_jwt, _) =
        refresh_token::create_refresh_token(&state.pool, &state.config, user.id, None, None, Some(&ip), Some(ua))
            .await?;

    let user_info: UserInfo = user.into();

    // Set HttpOnly cookies (access + refresh)
    let access_max_age = state.config.jwt_access_token_expire_minutes * 60;
    let access_cookie = format!(
        "token={}; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age={}",
        token, access_max_age
    );
    let refresh_cookie = auth_common::refresh_token_cookie(&state.config, &refresh_jwt);

    let body = Json(LoginResponse {
        user: user_info,
        token: token.clone(),
    });

    let mut response = body.into_response();
    response
        .headers_mut()
        .append(SET_COOKIE, access_cookie.parse().unwrap());
    response
        .headers_mut()
        .append(SET_COOKIE, refresh_cookie.parse().unwrap());

    Ok(response)
}

/// POST /api/auth/logout
pub async fn logout(State(state): State<AppState>, headers: axum::http::HeaderMap) -> Response {
    // Revoke refresh token if present
    let cookie_str = headers.get(http::header::COOKIE).and_then(|v| v.to_str().ok());

    if let Some(refresh_jwt) = auth_common::extract_refresh_token_from_cookie(cookie_str) {
        let token_hash = refresh_token::hash_refresh_token(&refresh_jwt, &state.config.jwt_secret);
        let _ = refresh_token::revoke_by_hash(&state.pool, &token_hash, "logout").await;
    }

    // Clear both cookies
    let clear_access = "token=; HttpOnly; Secure; SameSite=Lax; Path=/; Max-Age=0";
    let clear_refresh = auth_common::clear_refresh_token_cookie(&state.config);

    let mut response = Json(serde_json::json!({"message": "Logged out"})).into_response();
    response.headers_mut().append(SET_COOKIE, clear_access.parse().unwrap());
    response
        .headers_mut()
        .append(SET_COOKIE, clear_refresh.parse().unwrap());
    response
}

/// GET /api/auth/me — returns full user info including must_change_password from DB
pub async fn me(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let user = crate::services::user::get_by_id(&state.pool, auth_user.user_id).await?;

    Ok(Json(serde_json::json!({
        "id": user.id,
        "username": user.username,
        "role": user.role,
        "tenant_id": user.tenant_id,
        "email": user.email,
        "auth_method": user.auth_method,
        "must_change_password": user.must_change_password,
    })))
}

/// PUT /api/auth/change-password
pub async fn change_password(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<ChangePasswordRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if let Err(msg) = validate_password_strength(&req.new_password) {
        return Err(AppError::BadRequest(msg));
    }

    // Verify current password
    let user = crate::services::user::get_by_id(&state.pool, auth_user.user_id).await?;

    let current_pw = req.current_password.clone();
    let hash = user
        .password_hash
        .clone()
        .ok_or_else(|| AppError::BadRequest("This account uses OAuth — no password to change".to_string()))?;
    let valid = tokio::task::spawn_blocking(move || bcrypt::verify(current_pw, &hash))
        .await
        .map_err(|_| AppError::Internal("Password verification failed".to_string()))?
        .map_err(|_| AppError::Unauthorized("Current password is incorrect".to_string()))?;

    if !valid {
        return Err(AppError::Unauthorized("Current password is incorrect".to_string()));
    }

    let new_hash = crate::services::user::hash_password(req.new_password.clone()).await?;

    sqlx::query("UPDATE users SET password_hash = $1, must_change_password = false, updated_at = NOW() WHERE id = $2")
        .bind(&new_hash)
        .bind(auth_user.user_id)
        .execute(&state.pool)
        .await?;

    if let Some(email) = &user.email {
        crate::services::user::sync_password_to_cognito(&state.config, email, &req.new_password).await;
    }

    Ok(Json(serde_json::json!({"message": "Password changed successfully"})))
}

/// Fetch and validate an invite token — shared by validate and redeem handlers.
async fn fetch_invite_user(pool: &sqlx::PgPool, token: Uuid) -> AppResult<crate::models::user::User> {
    let user = sqlx::query_as::<_, crate::models::user::User>("SELECT * FROM users WHERE invite_token = $1")
        .bind(token)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Invalid or expired invite token".to_string()))?;

    if let Some(expires_at) = user.invite_token_expires_at
        && expires_at < chrono::Utc::now()
    {
        return Err(AppError::BadRequest("Invite token has expired".to_string()));
    }

    Ok(user)
}

/// GET /api/auth/invite/{token} — validate invite token (public, no auth)
pub async fn validate_invite(
    State(state): State<AppState>,
    Path(token): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let user = fetch_invite_user(&state.pool, token).await?;
    Ok(Json(serde_json::json!({
        "email": user.email,
        "username": user.username,
    })))
}

#[derive(serde::Deserialize)]
pub struct RedeemInviteRequest {
    pub password: String,
}

/// POST /api/auth/invite/{token}/redeem — set password and activate account (public, no auth)
pub async fn redeem_invite(
    State(state): State<AppState>,
    Path(token): Path<Uuid>,
    Json(req): Json<RedeemInviteRequest>,
) -> AppResult<Json<serde_json::Value>> {
    if let Err(msg) = validate_password_strength(&req.password) {
        return Err(AppError::BadRequest(msg));
    }

    let user = fetch_invite_user(&state.pool, token).await?;
    let new_hash = crate::services::user::hash_password(req.password.clone()).await?;

    sqlx::query(
        r#"UPDATE users SET
           password_hash = $1,
           invite_token = NULL,
           invite_token_expires_at = NULL,
           must_change_password = false,
           auth_method = 'local',
           updated_at = NOW()
           WHERE id = $2"#,
    )
    .bind(&new_hash)
    .bind(user.id)
    .execute(&state.pool)
    .await?;

    if let Some(email) = &user.email {
        crate::services::user::sync_password_to_cognito(&state.config, email, &req.password).await;
    }

    Ok(Json(
        serde_json::json!({"message": "Password set successfully. You can now log in."}),
    ))
}

/// Validate password complexity: min 8 chars, at least one uppercase, one lowercase, one digit.
fn validate_password_strength(password: &str) -> Result<(), String> {
    if password.len() < 8 {
        return Err("Password must be at least 8 characters".to_string());
    }
    if password.len() > 128 {
        return Err("Password must be at most 128 characters".to_string());
    }
    if !password.chars().any(|c| c.is_uppercase()) {
        return Err("Password must contain at least one uppercase letter".to_string());
    }
    if !password.chars().any(|c| c.is_lowercase()) {
        return Err("Password must contain at least one lowercase letter".to_string());
    }
    if !password.chars().any(|c| c.is_ascii_digit()) {
        return Err("Password must contain at least one digit".to_string());
    }
    Ok(())
}
