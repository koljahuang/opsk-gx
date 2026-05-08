use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::AppState;
use crate::error::AppResult;
use crate::middleware::auth::AuthUser;
use crate::models::pipeline::{CreatePipelineRepoRequest, PipelineRepo, UpdatePipelineRepoRequest};
use crate::services;
use crate::services::pipeline::TestConnectionResult;

// ── Frontend request DTOs (accept `token` from UI) ──────────────────────

#[derive(Debug, Deserialize)]
pub struct CreateRepoPayload {
    pub repo_id: String,
    pub name: String,
    pub repository: String,
    pub description: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Raw PAT token from frontend — stored in token_secret_arn for now
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateRepoPayload {
    pub name: Option<String>,
    pub repository: Option<String>,
    pub description: Option<String>,
    pub enabled: Option<bool>,
    pub token: Option<String>,
}

fn default_true() -> bool {
    true
}

/// GET /api/pipeline/repos
pub async fn list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<PipelineRepo>>> {
    let rows = services::pipeline::list(&state.pool, &auth_user).await?;
    Ok(Json(rows))
}

/// POST /api/pipeline/repos
pub async fn create(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(payload): Json<CreateRepoPayload>,
) -> AppResult<Json<PipelineRepo>> {
    let token = payload.token.clone();
    let req = CreatePipelineRepoRequest {
        repo_id: payload.repo_id,
        name: payload.name,
        repository: payload.repository,
        token_secret_arn: token.clone(),
        description: payload.description,
        enabled: payload.enabled,
    };
    let row = services::pipeline::create(&state.pool, &auth_user, req).await?;

    // Sync to ArgoCD (non-blocking, log on failure)
    if row.enabled
        && let Some(ref t) = token
        && !t.is_empty()
    {
        let id = row.id;
        let url = row.repository.clone();
        let t = t.clone();
        tokio::spawn(async move {
            if let Err(e) = services::argocd::sync_repo_secret(id, &url, &t).await {
                tracing::warn!("ArgoCD sync failed for repo {}: {}", id, e);
            }
        });
    }

    Ok(Json(row))
}

/// PUT /api/pipeline/repos/:id
pub async fn update(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateRepoPayload>,
) -> AppResult<Json<PipelineRepo>> {
    let token = payload.token.clone();
    let req = UpdatePipelineRepoRequest {
        name: payload.name,
        repository: payload.repository,
        token_secret_arn: token.clone(),
        description: payload.description,
        enabled: payload.enabled,
    };
    let row = services::pipeline::update(&state.pool, &auth_user, id, req).await?;

    // Sync to ArgoCD
    let is_enabled = row.enabled;
    let repo_id = row.id;
    let repo_url = row.repository.clone();
    // Resolve token: use new token if provided, otherwise try stored one
    let effective_token = token.or_else(|| row.token_secret_arn.clone());

    tokio::spawn(async move {
        if is_enabled
            && let Some(ref t) = effective_token
            && !t.is_empty()
        {
            if let Err(e) = services::argocd::sync_repo_secret(repo_id, &repo_url, t).await {
                tracing::warn!("ArgoCD sync failed for repo {}: {}", repo_id, e);
            }
            return;
        }
        // Disabled or no token → remove ArgoCD secret
        if let Err(e) = services::argocd::delete_repo_secret(repo_id).await {
            tracing::warn!("ArgoCD delete failed for repo {}: {}", repo_id, e);
        }
    });

    Ok(Json(row))
}

/// DELETE /api/pipeline/repos/:id
pub async fn delete(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    services::pipeline::delete(&state.pool, &auth_user, id).await?;

    // Remove ArgoCD secret
    tokio::spawn(async move {
        if let Err(e) = services::argocd::delete_repo_secret(id).await {
            tracing::warn!("ArgoCD delete failed for repo {}: {}", id, e);
        }
    });

    Ok(Json(serde_json::json!({"message": "Pipeline repo deleted"})))
}

/// POST /api/pipeline/repos/:id/test
pub async fn test_connection(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<TestConnectionResult>> {
    let result = services::pipeline::test_connection(&state.pool, &auth_user, id).await?;
    Ok(Json(result))
}

#[derive(Debug, Deserialize)]
pub struct TestInlineRequest {
    pub repository: String,
    pub token: Option<String>,
}

/// POST /api/pipeline/repos/test
pub async fn test_connection_inline(
    _auth_user: axum::Extension<AuthUser>,
    State(_state): State<AppState>,
    Json(req): Json<TestInlineRequest>,
) -> AppResult<Json<TestConnectionResult>> {
    let result = services::pipeline::test_connection_inline(&req.repository, req.token.as_deref()).await?;
    Ok(Json(result))
}
