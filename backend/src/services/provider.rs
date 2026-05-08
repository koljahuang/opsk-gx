use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::provider::{
    CreateProviderRequest, Provider, ProviderTypeOption, ProviderWithDefault, UpdateProviderRequest,
};
use crate::services::common::{require_non_empty, require_super_admin};

/// List providers visible to the current user, each with is_default flag.
/// super_admin: all global providers (is_default = false since it's per-tenant).
/// Others: only providers assigned to their tenant (with tenant-level is_default).
pub async fn list(pool: &PgPool, auth_user: &AuthUser) -> AppResult<Vec<ProviderWithDefault>> {
    let rows = if auth_user.is_super_admin() {
        let providers = sqlx::query_as::<_, Provider>("SELECT * FROM providers ORDER BY created_at")
            .fetch_all(pool)
            .await?;
        providers
            .into_iter()
            .map(|p| ProviderWithDefault {
                provider: p,
                is_default: false,
            })
            .collect()
    } else {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                String,
                serde_json::Value,
                Option<String>,
                chrono::DateTime<chrono::Utc>,
                chrono::DateTime<chrono::Utc>,
                bool,
            ),
        >(
            r#"SELECT p.id, p.name, p.provider_type, p.config, p.secret_arn, p.created_at, p.updated_at, tp.is_default
               FROM providers p
               JOIN tenant_providers tp ON p.id = tp.provider_id
               WHERE tp.tenant_id = $1
               ORDER BY tp.is_default DESC, p.created_at"#,
        )
        .bind(auth_user.tenant_id)
        .fetch_all(pool)
        .await?;

        rows.into_iter()
            .map(
                |(id, name, provider_type, config, secret_arn, created_at, updated_at, is_default)| {
                    ProviderWithDefault {
                        provider: Provider {
                            id,
                            name,
                            provider_type,
                            config,
                            secret_arn,
                            created_at,
                            updated_at,
                        },
                        is_default,
                    }
                },
            )
            .collect()
    };
    Ok(rows)
}

/// Return available provider types based on environment.
pub fn available_types(is_local: bool) -> Vec<ProviderTypeOption> {
    let mut types = Vec::new();

    if is_local {
        types.push(ProviderTypeOption {
            value: "bedrock".to_string(),
            label: "Amazon Bedrock".to_string(),
        });
    }

    types.push(ProviderTypeOption {
        value: "gateway".to_string(),
        label: "AI Gateway".to_string(),
    });

    types
}

/// Create a new global provider (super_admin only).
pub async fn create(pool: &PgPool, auth_user: &AuthUser, req: CreateProviderRequest) -> AppResult<Provider> {
    require_super_admin(auth_user, "create model cards")?;
    require_non_empty(&req.name, "Name")?;

    let row = sqlx::query_as::<_, Provider>(
        r#"INSERT INTO providers (name, provider_type, config)
           VALUES ($1, $2, $3)
           RETURNING *"#,
    )
    .bind(req.name.trim())
    .bind(&req.provider_type)
    .bind(&req.config)
    .fetch_one(pool)
    .await?;

    Ok(row)
}

/// Update an existing provider (super_admin only).
pub async fn update(pool: &PgPool, auth_user: &AuthUser, id: Uuid, req: UpdateProviderRequest) -> AppResult<Provider> {
    require_super_admin(auth_user, "update model cards")?;

    let row = sqlx::query_as::<_, Provider>(
        r#"UPDATE providers SET
           name = COALESCE($2, name),
           provider_type = COALESCE($3, provider_type),
           config = COALESCE($4, config),
           updated_at = NOW()
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(&req.name)
    .bind(&req.provider_type)
    .bind(&req.config)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Provider not found".to_string()))?;

    Ok(row)
}

/// Delete a provider (super_admin only).
pub async fn delete(pool: &PgPool, auth_user: &AuthUser, id: Uuid) -> AppResult<()> {
    require_super_admin(auth_user, "delete model cards")?;

    let result = sqlx::query("DELETE FROM providers WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Provider not found".to_string()));
    }

    Ok(())
}

// ─── Tenant assignment ─────────────────────────────────────────────

/// List providers assigned to a tenant (with is_default flag).
pub async fn list_by_tenant(
    pool: &PgPool,
    auth_user: &AuthUser,
    tenant_id: Uuid,
) -> AppResult<Vec<ProviderWithDefault>> {
    require_super_admin(auth_user, "manage tenant model cards")?;

    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            String,
            String,
            serde_json::Value,
            Option<String>,
            chrono::DateTime<chrono::Utc>,
            chrono::DateTime<chrono::Utc>,
            bool,
        ),
    >(
        r#"SELECT p.id, p.name, p.provider_type, p.config, p.secret_arn, p.created_at, p.updated_at, tp.is_default
           FROM providers p
           JOIN tenant_providers tp ON p.id = tp.provider_id
           WHERE tp.tenant_id = $1
           ORDER BY tp.is_default DESC, p.created_at"#,
    )
    .bind(tenant_id)
    .fetch_all(pool)
    .await?;

    let result = rows
        .into_iter()
        .map(
            |(id, name, provider_type, config, secret_arn, created_at, updated_at, is_default)| ProviderWithDefault {
                provider: Provider {
                    id,
                    name,
                    provider_type,
                    config,
                    secret_arn,
                    created_at,
                    updated_at,
                },
                is_default,
            },
        )
        .collect();

    Ok(result)
}

/// Assign providers to a tenant (replaces existing assignments).
/// Preserves is_default on providers that remain assigned.
pub async fn assign_to_tenant(
    pool: &PgPool,
    auth_user: &AuthUser,
    tenant_id: Uuid,
    provider_ids: Vec<Uuid>,
) -> AppResult<()> {
    require_super_admin(auth_user, "assign model cards to tenants")?;

    let mut tx = pool.begin().await?;

    // Remove existing assignments not in the new list
    sqlx::query("DELETE FROM tenant_providers WHERE tenant_id = $1 AND NOT (provider_id = ANY($2))")
        .bind(tenant_id)
        .bind(&provider_ids)
        .execute(&mut *tx)
        .await?;

    // Insert new assignments (skip existing via ON CONFLICT)
    for pid in &provider_ids {
        sqlx::query(
            r#"INSERT INTO tenant_providers (tenant_id, provider_id)
               VALUES ($1, $2)
               ON CONFLICT (tenant_id, provider_id) DO NOTHING"#,
        )
        .bind(tenant_id)
        .bind(pid)
        .execute(&mut *tx)
        .await?;
    }

    // If no default is set, auto-promote the first one
    let has_default = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM tenant_providers WHERE tenant_id = $1 AND is_default = true)",
    )
    .bind(tenant_id)
    .fetch_one(&mut *tx)
    .await?;

    if !has_default && !provider_ids.is_empty() {
        sqlx::query("UPDATE tenant_providers SET is_default = true WHERE tenant_id = $1 AND provider_id = $2")
            .bind(tenant_id)
            .bind(provider_ids[0])
            .execute(&mut *tx)
            .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Set the default provider for a tenant.
pub async fn set_tenant_default(
    pool: &PgPool,
    auth_user: &AuthUser,
    tenant_id: Uuid,
    provider_id: Uuid,
) -> AppResult<()> {
    require_super_admin(auth_user, "set default model card")?;

    // Verify the provider is assigned to this tenant
    let assigned = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM tenant_providers WHERE tenant_id = $1 AND provider_id = $2)",
    )
    .bind(tenant_id)
    .bind(provider_id)
    .fetch_one(pool)
    .await?;

    if !assigned {
        return Err(AppError::BadRequest(
            "Provider is not assigned to this tenant".to_string(),
        ));
    }

    // Unset all defaults, then set the new one
    sqlx::query("UPDATE tenant_providers SET is_default = false WHERE tenant_id = $1")
        .bind(tenant_id)
        .execute(pool)
        .await?;

    sqlx::query("UPDATE tenant_providers SET is_default = true WHERE tenant_id = $1 AND provider_id = $2")
        .bind(tenant_id)
        .bind(provider_id)
        .execute(pool)
        .await?;

    Ok(())
}

/// Count how many tenants a provider is assigned to (for super_admin card display).
pub async fn count_tenant_assignments(pool: &PgPool) -> AppResult<Vec<(Uuid, i64)>> {
    let rows =
        sqlx::query_as::<_, (Uuid, i64)>("SELECT provider_id, COUNT(*) FROM tenant_providers GROUP BY provider_id")
            .fetch_all(pool)
            .await?;
    Ok(rows)
}

// ─── Provider-centric tenant assignment ────────────────────────────

/// List tenants assigned to a specific provider (with is_default flag).
pub async fn list_tenants_for_provider(
    pool: &PgPool,
    auth_user: &AuthUser,
    provider_id: Uuid,
) -> AppResult<Vec<crate::models::provider::ProviderTenantAssignment>> {
    require_super_admin(auth_user, "view provider tenants")?;

    let rows = sqlx::query_as::<_, crate::models::provider::ProviderTenantAssignment>(
        r#"SELECT tp.tenant_id, t.name AS tenant_name, tp.is_default
           FROM tenant_providers tp
           JOIN tenants t ON t.id = tp.tenant_id
           WHERE tp.provider_id = $1
           ORDER BY t.name"#,
    )
    .bind(provider_id)
    .fetch_all(pool)
    .await?;

    Ok(rows)
}

/// Assign a provider to multiple tenants (provider-centric, replaces existing).
pub async fn assign_provider_to_tenants(
    pool: &PgPool,
    auth_user: &AuthUser,
    provider_id: Uuid,
    tenant_ids: Vec<Uuid>,
) -> AppResult<()> {
    require_super_admin(auth_user, "assign model card to tenants")?;

    let mut tx = pool.begin().await?;

    // Remove assignments not in the new list
    sqlx::query("DELETE FROM tenant_providers WHERE provider_id = $1 AND NOT (tenant_id = ANY($2))")
        .bind(provider_id)
        .bind(&tenant_ids)
        .execute(&mut *tx)
        .await?;

    // Insert new assignments (skip existing)
    for tid in &tenant_ids {
        sqlx::query(
            r#"INSERT INTO tenant_providers (tenant_id, provider_id)
               VALUES ($1, $2)
               ON CONFLICT (tenant_id, provider_id) DO NOTHING"#,
        )
        .bind(tid)
        .bind(provider_id)
        .execute(&mut *tx)
        .await?;
    }

    // For each newly added tenant, if it has no default, set this provider as default
    for tid in &tenant_ids {
        let has_default = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM tenant_providers WHERE tenant_id = $1 AND is_default = true)",
        )
        .bind(tid)
        .fetch_one(&mut *tx)
        .await?;

        if !has_default {
            sqlx::query("UPDATE tenant_providers SET is_default = true WHERE tenant_id = $1 AND provider_id = $2")
                .bind(tid)
                .bind(provider_id)
                .execute(&mut *tx)
                .await?;
        }
    }

    tx.commit().await?;
    Ok(())
}
