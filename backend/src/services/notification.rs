use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::AppResult;
use crate::models::notification::Notification;

/// Insert a single notification for a specific user.
/// If `tx` is provided, also broadcasts via SSE channel for real-time delivery.
#[allow(clippy::too_many_arguments)]
pub async fn notify_user(
    pool: &PgPool,
    user_id: Uuid,
    tenant_id: Option<Uuid>,
    event_type: &str,
    title: &str,
    description: &str,
    payload: serde_json::Value,
    related_id: Option<Uuid>,
    tx: Option<&broadcast::Sender<Notification>>,
) {
    let result = sqlx::query_as::<_, Notification>(
        r#"INSERT INTO notifications (user_id, tenant_id, event_type, title, description, payload, related_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING *"#,
    )
    .bind(user_id)
    .bind(tenant_id)
    .bind(event_type)
    .bind(title)
    .bind(description)
    .bind(&payload)
    .bind(related_id)
    .fetch_one(pool)
    .await;

    match result {
        Ok(notification) => {
            // Broadcast for real-time SSE delivery
            if let Some(tx) = tx {
                let _ = tx.send(notification);
            }
        }
        Err(e) => {
            tracing::error!("Failed to create notification for user {}: {}", user_id, e);
        }
    }
}

/// Notify all tenant admins (tenant_admin + super_admin) for a given tenant.
/// `exclude_user_id` — skip this user (e.g. the submitter themselves).
#[allow(clippy::too_many_arguments)]
pub async fn notify_tenant_admins(
    pool: &PgPool,
    tenant_id: Option<Uuid>,
    event_type: &str,
    title: &str,
    description: &str,
    payload: serde_json::Value,
    related_id: Option<Uuid>,
    exclude_user_id: Option<Uuid>,
    tx: Option<&broadcast::Sender<Notification>>,
) {
    // Find admin user_ids for this tenant
    let admin_ids: Vec<Uuid> = match tenant_id {
        Some(tid) => sqlx::query_scalar::<_, Uuid>(
            r#"SELECT id FROM users
                   WHERE (tenant_id = $1 AND role = 'tenant_admin')
                      OR role = 'super_admin'"#,
        )
        .bind(tid)
        .fetch_all(pool)
        .await
        .unwrap_or_default(),
        None => sqlx::query_scalar::<_, Uuid>("SELECT id FROM users WHERE role = 'super_admin'")
            .fetch_all(pool)
            .await
            .unwrap_or_default(),
    };

    for uid in admin_ids {
        // Skip the excluded user (e.g. don't notify submitter about their own submission)
        if exclude_user_id == Some(uid) {
            continue;
        }
        notify_user(
            pool,
            uid,
            tenant_id,
            event_type,
            title,
            description,
            payload.clone(),
            related_id,
            tx,
        )
        .await;
    }
}

/// Notify users who have a specific permission (via roles table).
/// Decoupled from hardcoded role names — follows RBAC.
#[allow(clippy::too_many_arguments)]
pub async fn notify_users_with_permission(
    pool: &PgPool,
    tenant_id: Option<Uuid>,
    permission: &str,
    event_type: &str,
    title: &str,
    description: &str,
    payload: serde_json::Value,
    related_id: Option<Uuid>,
    exclude_user_id: Option<Uuid>,
    tx: Option<&broadcast::Sender<Notification>>,
) {
    // Find users whose role grants the specified permission.
    // Uses JSONB containment: roles.permissions @> '["approval.approve"]'
    // Also matches wildcard "*" and category wildcard "approval.*"
    let perm_json = serde_json::json!([permission]);
    let category = permission.split('.').next().unwrap_or("");
    let wildcard_json = serde_json::json!([format!("{}.*", category)]);
    let star_json = serde_json::json!(["*"]);

    let user_ids: Vec<Uuid> = match tenant_id {
        Some(tid) => sqlx::query_scalar::<_, Uuid>(
            r#"SELECT u.id FROM users u
                   JOIN roles r ON u.role = r.name
                   WHERE u.tenant_id = $1
                     AND (r.permissions @> $2 OR r.permissions @> $3 OR r.permissions @> $4)"#,
        )
        .bind(tid)
        .bind(&perm_json)
        .bind(&wildcard_json)
        .bind(&star_json)
        .fetch_all(pool)
        .await
        .unwrap_or_default(),
        None => sqlx::query_scalar::<_, Uuid>(
            r#"SELECT u.id FROM users u
                   JOIN roles r ON u.role = r.name
                   WHERE r.permissions @> $1 OR r.permissions @> $2 OR r.permissions @> $3"#,
        )
        .bind(&perm_json)
        .bind(&wildcard_json)
        .bind(&star_json)
        .fetch_all(pool)
        .await
        .unwrap_or_default(),
    };

    for uid in user_ids {
        if exclude_user_id == Some(uid) {
            continue;
        }
        notify_user(
            pool,
            uid,
            tenant_id,
            event_type,
            title,
            description,
            payload.clone(),
            related_id,
            tx,
        )
        .await;
    }
}

/// List notifications for a user, newest first.
pub async fn list(pool: &PgPool, user_id: Uuid, limit: i64, offset: i64) -> AppResult<Vec<Notification>> {
    let notifications = sqlx::query_as::<_, Notification>(
        "SELECT * FROM notifications WHERE user_id = $1 ORDER BY created_at DESC LIMIT $2 OFFSET $3",
    )
    .bind(user_id)
    .bind(limit)
    .bind(offset)
    .fetch_all(pool)
    .await?;

    Ok(notifications)
}

/// Count unread notifications for a user.
pub async fn count_unread(pool: &PgPool, user_id: Uuid) -> AppResult<i64> {
    let count = sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM notifications WHERE user_id = $1 AND NOT is_read")
        .bind(user_id)
        .fetch_one(pool)
        .await?;

    Ok(count)
}

/// Mark a single notification as read. Returns true if updated.
pub async fn mark_read(pool: &PgPool, user_id: Uuid, id: Uuid) -> AppResult<bool> {
    let result = sqlx::query("UPDATE notifications SET is_read = true WHERE id = $1 AND user_id = $2 AND NOT is_read")
        .bind(id)
        .bind(user_id)
        .execute(pool)
        .await?;

    Ok(result.rows_affected() > 0)
}

/// Mark all notifications as read for a user.
pub async fn mark_all_read(pool: &PgPool, user_id: Uuid) -> AppResult<i64> {
    let result = sqlx::query("UPDATE notifications SET is_read = true WHERE user_id = $1 AND NOT is_read")
        .bind(user_id)
        .execute(pool)
        .await?;

    Ok(result.rows_affected() as i64)
}
