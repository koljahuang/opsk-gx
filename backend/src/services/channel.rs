use sqlx::PgPool;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::channel::{Channel, ChannelWithTenants, CreateChannelRequest, UpdateChannelRequest};
use crate::services::common::{require_non_empty, require_super_admin};

/// Fetch tenant IDs associated with a channel.
async fn get_tenant_ids(pool: &PgPool, channel_id: Uuid) -> AppResult<Vec<Uuid>> {
    let ids =
        sqlx::query_scalar::<_, Uuid>("SELECT tenant_id FROM channel_tenants WHERE channel_id = $1 ORDER BY tenant_id")
            .bind(channel_id)
            .fetch_all(pool)
            .await?;
    Ok(ids)
}

/// Replace the tenant associations for a channel (DELETE + INSERT).
async fn set_tenant_ids(pool: &PgPool, channel_id: Uuid, tenant_ids: &[Uuid]) -> AppResult<()> {
    sqlx::query("DELETE FROM channel_tenants WHERE channel_id = $1")
        .bind(channel_id)
        .execute(pool)
        .await?;
    for tid in tenant_ids {
        sqlx::query("INSERT INTO channel_tenants (channel_id, tenant_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(channel_id)
            .bind(tid)
            .execute(pool)
            .await?;
    }
    Ok(())
}

/// List all channels. Super admin only.
pub async fn list(pool: &PgPool, auth_user: &AuthUser) -> AppResult<Vec<ChannelWithTenants>> {
    require_super_admin(auth_user, "list channels")?;

    let rows = sqlx::query_as::<_, Channel>("SELECT * FROM channels ORDER BY name")
        .fetch_all(pool)
        .await?;

    // Batch-fetch all tenant associations in one query
    let channel_ids: Vec<Uuid> = rows.iter().map(|c| c.id).collect();
    let all_links = sqlx::query_as::<_, (Uuid, Uuid)>(
        "SELECT channel_id, tenant_id FROM channel_tenants WHERE channel_id = ANY($1) ORDER BY channel_id, tenant_id",
    )
    .bind(&channel_ids)
    .fetch_all(pool)
    .await?;

    let mut tenant_map: std::collections::HashMap<Uuid, Vec<Uuid>> = std::collections::HashMap::new();
    for (cid, tid) in all_links {
        tenant_map.entry(cid).or_default().push(tid);
    }

    let result = rows
        .into_iter()
        .map(|ch| {
            let tenant_ids = tenant_map.remove(&ch.id).unwrap_or_default();
            ChannelWithTenants {
                channel: ch,
                tenant_ids,
            }
        })
        .collect();
    Ok(result)
}

/// Create a new channel and link it to the specified tenants. Super admin only.
pub async fn create(pool: &PgPool, auth_user: &AuthUser, req: CreateChannelRequest) -> AppResult<ChannelWithTenants> {
    require_super_admin(auth_user, "create channels")?;
    require_non_empty(&req.name, "Name")?;
    require_non_empty(&req.platform, "Platform")?;

    if req.tenant_ids.is_empty() {
        return Err(AppError::BadRequest("At least one tenant is required".to_string()));
    }

    let channel = sqlx::query_as::<_, Channel>(
        r#"INSERT INTO channels (platform, name, credentials, settings, enabled)
           VALUES ($1, $2, $3, $4, $5)
           RETURNING *"#,
    )
    .bind(&req.platform)
    .bind(&req.name)
    .bind(&req.credentials)
    .bind(&req.settings)
    .bind(req.enabled)
    .fetch_one(pool)
    .await?;

    let tenant_ids = req.tenant_ids.clone();
    set_tenant_ids(pool, channel.id, &tenant_ids).await?;

    Ok(ChannelWithTenants { channel, tenant_ids })
}

/// Update an existing channel. Super admin only.
pub async fn update(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    req: UpdateChannelRequest,
) -> AppResult<ChannelWithTenants> {
    require_super_admin(auth_user, "update channels")?;

    let channel = sqlx::query_as::<_, Channel>(
        r#"UPDATE channels SET
           platform = COALESCE($2, platform),
           name = COALESCE($3, name),
           credentials = COALESCE($4, credentials),
           settings = COALESCE($5, settings),
           enabled = COALESCE($6, enabled),
           updated_at = NOW()
           WHERE id = $1
           RETURNING *"#,
    )
    .bind(id)
    .bind(&req.platform)
    .bind(&req.name)
    .bind(&req.credentials)
    .bind(&req.settings)
    .bind(req.enabled)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Channel not found".to_string()))?;

    // Update tenant associations if provided
    if let Some(ref tenant_ids) = req.tenant_ids {
        if tenant_ids.is_empty() {
            return Err(AppError::BadRequest("At least one tenant is required".to_string()));
        }
        set_tenant_ids(pool, id, tenant_ids).await?;
    }

    let tenant_ids = get_tenant_ids(pool, id).await?;
    Ok(ChannelWithTenants { channel, tenant_ids })
}

/// Delete a channel by ID. Super admin only.
pub async fn delete(pool: &PgPool, auth_user: &AuthUser, id: Uuid) -> AppResult<()> {
    require_super_admin(auth_user, "delete channels")?;

    let result = sqlx::query("DELETE FROM channels WHERE id = $1")
        .bind(id)
        .execute(pool)
        .await?;

    if result.rows_affected() == 0 {
        return Err(AppError::NotFound("Channel not found".to_string()));
    }

    Ok(())
}
