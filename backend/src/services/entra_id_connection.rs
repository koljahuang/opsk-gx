use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::entra_id_connection::{
    CreateEntraIdConnectionRequest, EntraIdConnection, UpdateEntraIdConnectionRequest,
};
use crate::services::common::{map_constraint_error, require_non_empty, require_super_admin};

const CONSTRAINT_MAPPINGS: &[(&str, &str)] = &[(
    "entra_id_connections_entra_tenant_id_key",
    "A connection for this Entra ID tenant already exists",
)];

/// List all Entra ID connections (super_admin only).
pub async fn list(pool: &PgPool, auth_user: &AuthUser) -> AppResult<Vec<EntraIdConnection>> {
    require_super_admin(auth_user, "manage SSO connections")?;

    let connections = sqlx::query_as::<_, EntraIdConnection>("SELECT * FROM entra_id_connections ORDER BY name")
        .fetch_all(pool)
        .await?;

    Ok(connections)
}

/// Get a single connection by ID (super_admin only).
pub async fn get(pool: &PgPool, auth_user: &AuthUser, id: Uuid) -> AppResult<EntraIdConnection> {
    require_super_admin(auth_user, "manage SSO connections")?;

    sqlx::query_as::<_, EntraIdConnection>("SELECT * FROM entra_id_connections WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Connection not found".to_string()))
}

/// Create a new Entra ID connection (super_admin only).
pub async fn create(
    pool: &PgPool,
    auth_user: &AuthUser,
    req: CreateEntraIdConnectionRequest,
) -> AppResult<EntraIdConnection> {
    require_super_admin(auth_user, "create SSO connections")?;
    require_non_empty(&req.name, "name")?;
    require_non_empty(&req.entra_tenant_id, "entra_tenant_id")?;
    require_non_empty(&req.client_id, "client_id")?;
    require_non_empty(&req.client_secret, "client_secret")?;

    let default_role = req.default_role.as_deref().unwrap_or("member");
    if default_role != "super_admin" && default_role != "tenant_admin" && default_role != "member" {
        return Err(AppError::BadRequest(
            "default_role must be 'super_admin', 'tenant_admin', or 'member'".to_string(),
        ));
    }

    let allowed_domains = req.allowed_domains.unwrap_or_default();

    let connection = sqlx::query_as::<_, EntraIdConnection>(
        r#"INSERT INTO entra_id_connections
           (name, entra_tenant_id, client_id, client_secret, tenant_id, auto_provision, default_role, allowed_domains)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
           RETURNING *"#,
    )
    .bind(&req.name)
    .bind(&req.entra_tenant_id)
    .bind(&req.client_id)
    .bind(&req.client_secret)
    .bind(req.tenant_id)
    .bind(req.auto_provision.unwrap_or(true))
    .bind(default_role)
    .bind(&allowed_domains)
    .fetch_one(pool)
    .await
    .map_err(|e| map_constraint_error(e, CONSTRAINT_MAPPINGS))?;

    Ok(connection)
}

/// Update an existing Entra ID connection (super_admin only).
pub async fn update(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    req: UpdateEntraIdConnectionRequest,
) -> AppResult<EntraIdConnection> {
    require_super_admin(auth_user, "update SSO connections")?;

    if let Some(ref role) = req.default_role
        && role != "super_admin"
        && role != "tenant_admin"
        && role != "member"
    {
        return Err(AppError::BadRequest(
            "default_role must be 'super_admin', 'tenant_admin', or 'member'".to_string(),
        ));
    }

    let connection = sqlx::query_as::<_, EntraIdConnection>(
        r#"UPDATE entra_id_connections SET
           name = COALESCE($2, name),
           entra_tenant_id = COALESCE($3, entra_tenant_id),
           client_id = COALESCE($4, client_id),
           client_secret = COALESCE($5, client_secret),
           tenant_id = COALESCE($6, tenant_id),
           auto_provision = COALESCE($7, auto_provision),
           default_role = COALESCE($8, default_role),
           enabled = COALESCE($9, enabled),
           allowed_domains = COALESCE($10, allowed_domains),
           updated_at = NOW()
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.entra_tenant_id)
    .bind(&req.client_id)
    .bind(&req.client_secret)
    .bind(req.tenant_id)
    .bind(req.auto_provision)
    .bind(&req.default_role)
    .bind(req.enabled)
    .bind(&req.allowed_domains)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Connection not found".to_string()))?;

    Ok(connection)
}

/// Delete an Entra ID connection (super_admin only).
pub async fn delete(pool: &PgPool, auth_user: &AuthUser, id: Uuid) -> AppResult<()> {
    require_super_admin(auth_user, "delete SSO connections")?;

    let result = sqlx::query("DELETE FROM entra_id_connections WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Connection not found".to_string()));
    }

    Ok(())
}

/// Discover an SSO connection by email domain. No auth required (pre-login).
pub async fn discover_by_email(pool: &PgPool, email: &str) -> AppResult<Option<EntraIdConnection>> {
    let domain = email
        .rsplit_once('@')
        .map(|(_, d)| d.to_lowercase())
        .ok_or_else(|| AppError::BadRequest("Invalid email".to_string()))?;

    let connection = sqlx::query_as::<_, EntraIdConnection>(
        "SELECT * FROM entra_id_connections WHERE enabled = true AND $1 = ANY(allowed_domains) LIMIT 1",
    )
    .bind(&domain)
    .fetch_optional(pool)
    .await?;

    Ok(connection)
}

/// Internal lookup by ID (no auth check, used by OAuth callback).
pub async fn get_by_id(pool: &PgPool, id: Uuid) -> AppResult<Option<EntraIdConnection>> {
    let connection = sqlx::query_as::<_, EntraIdConnection>("SELECT * FROM entra_id_connections WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?;

    Ok(connection)
}

/// Check if any enabled SSO connections exist.
pub async fn has_enabled_connections(pool: &PgPool) -> bool {
    sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM entra_id_connections WHERE enabled = true)")
        .fetch_one(pool)
        .await
        .unwrap_or(false)
}
