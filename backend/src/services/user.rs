use sqlx::PgPool;
use uuid::Uuid;

use crate::config::AppConfig;
use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::user::{CreateUserRequest, InviteUserRequest, UpdateUserRequest, User, UserInfo};
use crate::services::common::{require_super_admin, tenant_filter};

/// Fetch a user by ID.
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> AppResult<User> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("User not found".to_string()))
}

/// Hash a password with bcrypt on a blocking thread.
pub async fn hash_password(password: String) -> AppResult<String> {
    tokio::task::spawn_blocking(move || bcrypt::hash(password, 10))
        .await
        .map_err(|_| AppError::Internal("Password hashing failed".to_string()))?
        .map_err(|e| AppError::Internal(format!("Bcrypt error: {e}")))
}

/// Sync a password to Cognito (best-effort, logs errors).
pub async fn sync_password_to_cognito(config: &AppConfig, email: &str, password: &str) {
    if let Some(cognito_config) = &config.cognito_oauth
        && let Err(e) = crate::services::cognito_admin::set_cognito_password(cognito_config, email, password).await
    {
        tracing::error!("Failed to sync password to Cognito for {}: {}", email, e);
    }
}

/// List users visible to the authenticated user.
/// Super admins see all; members see only their tenant.
pub async fn list(pool: &PgPool, auth_user: &AuthUser) -> AppResult<Vec<UserInfo>> {
    let users = match tenant_filter(auth_user) {
        None => {
            sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY username")
                .fetch_all(pool)
                .await?
        }
        Some(tid) => {
            sqlx::query_as::<_, User>("SELECT * FROM users WHERE tenant_id = $1 ORDER BY username")
                .bind(tid)
                .fetch_all(pool)
                .await?
        }
    };

    Ok(users.into_iter().map(UserInfo::from).collect())
}

/// Create a new user (super_admin only).
pub async fn create(pool: &PgPool, auth_user: &AuthUser, req: CreateUserRequest) -> AppResult<UserInfo> {
    require_super_admin(auth_user, "create users")?;

    if req.username.trim().is_empty() || req.password.len() < 8 {
        return Err(AppError::BadRequest(
            "Username required, password must be at least 8 characters".to_string(),
        ));
    }

    // Input length validation — prevent storage exhaustion
    if req.username.len() > 128 {
        return Err(AppError::BadRequest(
            "Username must be at most 128 characters".to_string(),
        ));
    }
    if let Some(ref email) = req.email
        && email.len() > 255
    {
        return Err(AppError::BadRequest("Email must be at most 255 characters".to_string()));
    }

    if req.role != "super_admin" && req.role != "tenant_admin" && req.role != "member" {
        return Err(AppError::BadRequest(
            "Role must be 'super_admin', 'tenant_admin', or 'member'".to_string(),
        ));
    }

    if req.role != "super_admin" && req.tenant_id.is_none() {
        return Err(AppError::BadRequest(
            "tenant_id is required for tenant_admin and member roles".to_string(),
        ));
    }

    let password_hash = hash_password(req.password.clone()).await?;

    let user = sqlx::query_as::<_, User>(
        r#"INSERT INTO users (username, password_hash, role, tenant_id, email)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING *"#,
    )
    .bind(&req.username)
    .bind(&password_hash)
    .bind(&req.role)
    .bind(req.tenant_id)
    .bind(&req.email)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e
            && db_err.constraint() == Some("users_username_key")
        {
            return AppError::Conflict("Username already exists".to_string());
        }
        AppError::Database(e)
    })?;

    Ok(UserInfo::from(user))
}

/// Update an existing user (super_admin only).
pub async fn update(pool: &PgPool, auth_user: &AuthUser, id: Uuid, req: UpdateUserRequest) -> AppResult<UserInfo> {
    require_super_admin(auth_user, "update users")?;

    if let Some(ref role) = req.role
        && role != "super_admin"
        && role != "tenant_admin"
        && role != "member"
    {
        return Err(AppError::BadRequest(
            "Role must be 'super_admin', 'tenant_admin', or 'member'".to_string(),
        ));
    }

    let password_hash = match &req.password {
        Some(pw) => {
            if pw.len() < 8 {
                return Err(AppError::BadRequest(
                    "Password must be at least 8 characters".to_string(),
                ));
            }
            Some(hash_password(pw.clone()).await?)
        }
        None => None,
    };

    let user = sqlx::query_as::<_, User>(
        r#"UPDATE users SET
           username = COALESCE($2, username),
           password_hash = COALESCE($3, password_hash),
           role = COALESCE($4, role),
           tenant_id = COALESCE($5, tenant_id),
           email = COALESCE($6, email),
           is_active = COALESCE($7, is_active),
           updated_at = NOW()
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(&req.username)
    .bind(&password_hash)
    .bind(&req.role)
    .bind(req.tenant_id)
    .bind(&req.email)
    .bind(req.is_active)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("User not found".to_string()))?;

    Ok(UserInfo::from(user))
}

/// Delete a user by ID (super_admin only, cannot delete self).
pub async fn delete(pool: &PgPool, auth_user: &AuthUser, id: Uuid) -> AppResult<()> {
    require_super_admin(auth_user, "delete users")?;

    if auth_user.user_id == id {
        return Err(AppError::BadRequest("Cannot delete yourself".to_string()));
    }

    let result = sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("User not found".to_string()));
    }

    Ok(())
}

/// Generate a random temporary password (16 chars, alphanumeric + symbols).
fn generate_temp_password() -> String {
    use rand::Rng;
    const UPPER: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LOWER: &[u8] = b"abcdefghjkmnpqrstuvwxyz";
    const DIGITS: &[u8] = b"23456789";
    const SYMBOLS: &[u8] = b"!@#$%^&*";
    const ALL: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghjkmnpqrstuvwxyz23456789!@#$%^&*";

    let mut rng = rand::rng();
    let mut chars: Vec<char> = Vec::with_capacity(16);

    // Guarantee at least one of each category (Cognito policy)
    chars.push(UPPER[rng.random_range(0..UPPER.len())] as char);
    chars.push(LOWER[rng.random_range(0..LOWER.len())] as char);
    chars.push(DIGITS[rng.random_range(0..DIGITS.len())] as char);
    chars.push(SYMBOLS[rng.random_range(0..SYMBOLS.len())] as char);

    // Fill remaining with random from full set
    for _ in 0..12 {
        chars.push(ALL[rng.random_range(0..ALL.len())] as char);
    }

    // Shuffle so the guaranteed chars aren't always at the start
    use rand::seq::SliceRandom;
    chars.shuffle(&mut rng);
    chars.into_iter().collect()
}

/// Build the frontend invite URL from request origin or config.
fn build_invite_link(config: &AppConfig, token: &Uuid) -> String {
    let base = config
        .allowed_origins
        .first()
        .cloned()
        .unwrap_or_else(|| "http://localhost:3000".to_string());
    format!("{}/auth/invite?token={}", base, token)
}

/// Invite a user by email (super_admin only).
/// Creates Cognito user (if configured) → inserts DB with invite_token.
/// Returns UserInfo + invite_link + temp_password for admin to share manually.
pub async fn invite(
    pool: &PgPool,
    config: &AppConfig,
    auth_user: &AuthUser,
    req: InviteUserRequest,
) -> AppResult<serde_json::Value> {
    require_super_admin(auth_user, "invite users")?;

    let email = req.email.trim().to_lowercase();
    if email.is_empty() || !email.contains('@') {
        return Err(AppError::BadRequest("Valid email is required".to_string()));
    }

    let role = req.role.as_deref().unwrap_or("member");
    if role != "super_admin" && role != "tenant_admin" && role != "member" {
        return Err(AppError::BadRequest(
            "Role must be 'super_admin', 'tenant_admin', or 'member'".to_string(),
        ));
    }

    if role != "super_admin" && req.tenant_id.is_none() {
        return Err(AppError::BadRequest(
            "tenant_id is required for tenant_admin and member roles".to_string(),
        ));
    }

    let temp_password = generate_temp_password();
    let invite_token = Uuid::new_v4();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(72);

    // Create Cognito user + hash password in parallel
    let cognito_fut = async {
        if let Some(cognito_config) = &config.cognito_oauth {
            crate::services::cognito_admin::create_cognito_user(cognito_config, &email, &temp_password)
                .await
                .map(Some)
        } else {
            Ok(None)
        }
    };
    let (cognito_sub, password_hash) = tokio::try_join!(cognito_fut, hash_password(temp_password.clone()))?;

    // Insert user into DB
    let user = sqlx::query_as::<_, User>(
        r#"INSERT INTO users (username, email, role, tenant_id, auth_method, password_hash, cognito_sub,
                              invite_token, invite_token_expires_at, must_change_password)
           VALUES ($1, $2, $3, $4, 'invited', $5, $6, $7, $8, true)
           RETURNING *"#,
    )
    .bind(&email)
    .bind(&email)
    .bind(role)
    .bind(req.tenant_id)
    .bind(&password_hash)
    .bind(&cognito_sub)
    .bind(invite_token)
    .bind(expires_at)
    .fetch_one(pool)
    .await
    .map_err(|e| {
        if let sqlx::Error::Database(ref db_err) = e
            && db_err.constraint() == Some("users_username_key")
        {
            return AppError::Conflict("A user with this email already exists".to_string());
        }
        AppError::Database(e)
    })?;

    let invite_link = build_invite_link(config, &invite_token);

    let user_info = UserInfo::from(user);
    let mut response = serde_json::to_value(&user_info).map_err(|e| AppError::Internal(e.to_string()))?;
    response["invite_link"] = serde_json::Value::String(invite_link);

    Ok(response)
}

/// Resend the invite email for a pending user.
pub async fn resend_invite(
    pool: &PgPool,
    config: &AppConfig,
    auth_user: &AuthUser,
    user_id: Uuid,
) -> AppResult<serde_json::Value> {
    require_super_admin(auth_user, "resend invites")?;

    let user = get_by_id(pool, user_id).await?;

    if user.auth_method != "invited" {
        return Err(AppError::BadRequest("User is not in invited state".to_string()));
    }

    let email = user
        .email
        .as_deref()
        .ok_or_else(|| AppError::Internal("Invited user has no email".to_string()))?;

    // Generate new temp password + token
    let temp_password = generate_temp_password();
    let invite_token = Uuid::new_v4();
    let expires_at = chrono::Utc::now() + chrono::Duration::hours(72);

    let (password_hash, _) = tokio::join!(
        hash_password(temp_password.clone()),
        sync_password_to_cognito(config, email, &temp_password)
    );
    let password_hash = password_hash?;

    // Update DB
    sqlx::query(
        r#"UPDATE users SET password_hash = $1, invite_token = $2,
           invite_token_expires_at = $3, updated_at = NOW()
           WHERE id = $4"#,
    )
    .bind(&password_hash)
    .bind(invite_token)
    .bind(expires_at)
    .bind(user_id)
    .execute(pool)
    .await?;

    let invite_link = build_invite_link(config, &invite_token);

    Ok(serde_json::json!({
        "message": "Invite resent",
        "invite_link": invite_link,
    }))
}
