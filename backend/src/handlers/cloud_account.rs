use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use sqlx;

use crate::AppState;
use crate::error::AppResult;
use crate::middleware::auth::AuthUser;
use crate::models::cloud_account::{CloudAccount, CreateCloudAccountRequest, UpdateCloudAccountRequest};
use crate::services;
use crate::services::cloud_account::{OrgSyncResult, TestConnectionResult};

/// GET /api/accounts
pub async fn list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<CloudAccount>>> {
    let accounts = services::cloud_account::list(&state.pool, &auth_user).await?;
    Ok(Json(accounts))
}

/// POST /api/accounts
pub async fn create(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<CreateCloudAccountRequest>,
) -> AppResult<Json<CloudAccount>> {
    let discover_org = req.discover_org;
    let provider = req.provider.clone();
    let profile = req.profile.clone();
    let account = services::cloud_account::create(&state.pool, &auth_user, req).await?;

    if discover_org && provider == "aws" {
        let pool = state.pool.clone();
        let tid = account.tenant_id;
        tokio::spawn(async move {
            services::cloud_account::discover_org_background(&pool, profile.as_deref(), tid).await;
        });
    }
    Ok(Json(account))
}

/// PUT /api/accounts/:id
pub async fn update(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateCloudAccountRequest>,
) -> AppResult<Json<CloudAccount>> {
    let account = services::cloud_account::update(&state.pool, &auth_user, id, req).await?;
    Ok(Json(account))
}

/// DELETE /api/accounts/:id
pub async fn delete(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    services::cloud_account::delete(&state.pool, &auth_user, id).await?;
    Ok(Json(serde_json::json!({"message": "Cloud account deleted"})))
}

/// POST /api/accounts/discover
/// Fire-and-forget: spawns background task and returns 202 immediately.
pub async fn discover(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let pool = state.pool.clone();
    let tenant_id = auth_user.tenant_id;
    tokio::spawn(async move {
        match services::cloud_account::discover_for_tenant(&pool, tenant_id).await {
            Ok(accounts) => {
                tracing::info!(count = accounts.len(), "Background discover completed");
            }
            Err(e) => {
                tracing::error!("Background discover failed: {}", e);
            }
        }
    });
    Ok(Json(serde_json::json!({"status": "discovering"})))
}

/// POST /api/accounts/:id/discover-org
/// Fire-and-forget: spawns background task and returns 202 immediately.
pub async fn discover_org(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    // Validate access before spawning background task
    let account = sqlx::query_as::<_, CloudAccount>("SELECT * FROM cloud_accounts WHERE id = $1")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(|| crate::error::AppError::NotFound("Cloud account not found".to_string()))?;

    if !auth_user.is_super_admin() && account.tenant_id != auth_user.tenant_id {
        return Err(crate::error::AppError::Forbidden("Access denied".to_string()));
    }
    if account.provider != "aws" {
        return Err(crate::error::AppError::BadRequest(
            "Organization discovery only supported for AWS accounts".to_string(),
        ));
    }

    let pool = state.pool.clone();
    let profile = account.profile.clone();
    let tenant_id = account.tenant_id;
    tokio::spawn(async move {
        match services::cloud_account::discover_org_by_profile(&pool, profile.as_deref(), tenant_id).await {
            Ok(accounts) => {
                tracing::info!(count = accounts.len(), "Background discover-org completed");
            }
            Err(e) => {
                tracing::error!("Background discover-org failed: {}", e);
            }
        }
    });
    Ok(Json(serde_json::json!({"status": "discovering"})))
}

/// POST /api/accounts/sync
pub async fn sync(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<OrgSyncResult>> {
    let result = services::cloud_account::sync_org_accounts(&state.pool, auth_user.tenant_id)
        .await
        .map_err(|e| crate::error::AppError::Internal(format!("Org sync failed: {e}")))?;
    Ok(Json(result))
}

/// POST /api/accounts/:id/test
pub async fn test_connection(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<TestConnectionResult>> {
    let result = services::cloud_account::test_connection(&state.pool, &auth_user, id).await?;
    Ok(Json(result))
}

/// POST /api/accounts/seed-mock
pub async fn seed_mock(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<CloudAccount>>> {
    let accounts = services::cloud_account::seed_mock(&state.pool, &auth_user).await?;
    Ok(Json(accounts))
}
