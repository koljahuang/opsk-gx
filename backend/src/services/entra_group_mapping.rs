use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::entra_group_mapping::{
    AccountAccessEntry, CreateEntraGroupMappingRequest, EntraGroupMapping, UpdateEntraGroupMappingRequest,
};
use crate::services::common::{map_constraint_error, require_non_empty, require_super_admin};

const CONSTRAINT_MAPPINGS: &[(&str, &str)] = &[(
    "entra_group_mappings_group_id_key",
    "A mapping for this group ID already exists",
)];

/// List all Entra group mappings (super_admin only).
pub async fn list(pool: &PgPool, auth_user: &AuthUser) -> AppResult<Vec<EntraGroupMapping>> {
    require_super_admin(auth_user, "manage group mappings")?;

    let mappings =
        sqlx::query_as::<_, EntraGroupMapping>("SELECT * FROM entra_group_mappings ORDER BY group_name, group_id")
            .fetch_all(pool)
            .await?;

    Ok(mappings)
}

/// Create a new Entra group mapping (super_admin only).
pub async fn create(
    pool: &PgPool,
    auth_user: &AuthUser,
    req: CreateEntraGroupMappingRequest,
) -> AppResult<EntraGroupMapping> {
    require_super_admin(auth_user, "create group mappings")?;
    require_non_empty(&req.group_id, "group_id")?;

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

    let account_access = req.account_access.unwrap_or_else(|| serde_json::json!([]));

    let mapping = sqlx::query_as::<_, EntraGroupMapping>(
        r#"INSERT INTO entra_group_mappings (group_id, group_name, role, tenant_id, account_access)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING *"#,
    )
    .bind(&req.group_id)
    .bind(req.group_name.as_deref().unwrap_or(""))
    .bind(role)
    .bind(req.tenant_id)
    .bind(&account_access)
    .fetch_one(pool)
    .await
    .map_err(|e| map_constraint_error(e, CONSTRAINT_MAPPINGS))?;

    Ok(mapping)
}

/// Update an existing Entra group mapping (super_admin only).
pub async fn update(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    req: UpdateEntraGroupMappingRequest,
) -> AppResult<EntraGroupMapping> {
    require_super_admin(auth_user, "update group mappings")?;

    if let Some(ref role) = req.role
        && role != "super_admin"
        && role != "tenant_admin"
        && role != "member"
    {
        return Err(AppError::BadRequest(
            "Role must be 'super_admin', 'tenant_admin', or 'member'".to_string(),
        ));
    }

    let mapping = sqlx::query_as::<_, EntraGroupMapping>(
        r#"UPDATE entra_group_mappings SET
           group_id = COALESCE($2, group_id),
           group_name = COALESCE($3, group_name),
           role = COALESCE($4, role),
           tenant_id = COALESCE($5, tenant_id),
           account_access = COALESCE($6, account_access),
           updated_at = NOW()
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(&req.group_id)
    .bind(&req.group_name)
    .bind(&req.role)
    .bind(req.tenant_id)
    .bind(&req.account_access)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Group mapping not found".to_string()))?;

    Ok(mapping)
}

/// Delete an Entra group mapping by ID (super_admin only).
pub async fn delete(pool: &PgPool, auth_user: &AuthUser, id: Uuid) -> AppResult<()> {
    require_super_admin(auth_user, "delete group mappings")?;

    let result = sqlx::query("DELETE FROM entra_group_mappings WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Group mapping not found".to_string()));
    }

    Ok(())
}

/// Apply Entra group memberships to a user.
///
/// - Finds all matching mappings for the user's group IDs
/// - Highest-privilege role wins (super_admin > member)
/// - First tenant_id wins for member role
/// - Collects all account_access entries, dedup (admin > readonly per account)
/// - Updates user role/tenant_id and upserts user_account_access rows
pub async fn apply_group_mappings(pool: &PgPool, user_id: Uuid, group_ids: &[String]) -> AppResult<()> {
    if group_ids.is_empty() {
        return Ok(());
    }

    // Fetch all matching mappings in one query
    let mappings =
        sqlx::query_as::<_, EntraGroupMapping>("SELECT * FROM entra_group_mappings WHERE group_id = ANY($1)")
            .bind(group_ids)
            .fetch_all(pool)
            .await?;

    if mappings.is_empty() {
        return Ok(());
    }

    // Determine highest-privilege role and first tenant_id
    // Priority: super_admin > tenant_admin > member
    let mut best_role = "member";
    let mut best_tenant_id: Option<Uuid> = None;

    for m in &mappings {
        if m.role == "super_admin" {
            best_role = "super_admin";
        } else if m.role == "tenant_admin" && best_role != "super_admin" {
            best_role = "tenant_admin";
        }
        if best_tenant_id.is_none() && m.tenant_id.is_some() {
            best_tenant_id = m.tenant_id;
        }
    }

    // super_admin has no tenant
    let final_tenant_id = if best_role == "super_admin" {
        None
    } else {
        best_tenant_id
    };

    // Update user role and tenant
    sqlx::query("UPDATE users SET role = $1, tenant_id = $2, updated_at = NOW() WHERE id = $3")
        .bind(best_role)
        .bind(final_tenant_id)
        .bind(user_id)
        .execute(pool)
        .await?;

    // Collect account_access entries from all mappings, dedup (admin wins over readonly)
    let mut account_roles: std::collections::HashMap<Uuid, String> = std::collections::HashMap::new();

    for m in &mappings {
        if let Ok(entries) = serde_json::from_value::<Vec<AccountAccessEntry>>(m.account_access.clone()) {
            for entry in entries {
                let current = account_roles.get(&entry.account_id);
                // admin > readonly
                if current.is_none() || (current == Some(&"readonly".to_string()) && entry.role == "admin") {
                    account_roles.insert(entry.account_id, entry.role);
                }
            }
        }
    }

    // Batch upsert account access rows
    if !account_roles.is_empty() {
        let account_ids: Vec<Uuid> = account_roles.keys().copied().collect();
        let roles: Vec<&str> = account_ids.iter().map(|id| account_roles[id].as_str()).collect();
        let user_ids: Vec<Uuid> = vec![user_id; account_ids.len()];

        sqlx::query(
            r#"INSERT INTO user_account_access (user_id, account_id, role)
               SELECT * FROM UNNEST($1::uuid[], $2::uuid[], $3::text[])
               ON CONFLICT (user_id, account_id) DO UPDATE SET role = EXCLUDED.role"#,
        )
        .bind(&user_ids)
        .bind(&account_ids)
        .bind(&roles)
        .execute(pool)
        .await?;
    }

    Ok(())
}
