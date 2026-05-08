use axum::{
    Json,
    extract::{Path, Query, State},
};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppResult;
use crate::middleware::auth::AuthUser;
use crate::models::approval::{
    Approval, ApprovalListQuery, CreateApprovalRequest, MarkResultRequest, UpdateApprovalRequest,
};
use crate::services;

/// GET /api/approvals
pub async fn list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Query(query): Query<ApprovalListQuery>,
) -> AppResult<Json<Vec<Approval>>> {
    let approvals = services::approval::list(&state.pool, &auth_user, query.status.as_deref()).await?;
    Ok(Json(approvals))
}

/// GET /api/approvals/count — pending approval count for sidebar badge
pub async fn count(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let count = services::approval::count_pending(&state.pool, &auth_user).await?;
    Ok(Json(serde_json::json!({ "count": count })))
}

/// POST /api/approvals — create a pending approval (Agent-initiated)
pub async fn create(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<CreateApprovalRequest>,
) -> AppResult<Json<Approval>> {
    let approval = services::approval::create(
        &state.pool,
        &auth_user,
        &req.command,
        req.reason.as_deref(),
        req.plan_detail.as_ref(),
        &state.notification_tx,
    )
    .await?;
    Ok(Json(approval))
}

/// PUT /api/approvals/:id — update jira_key (Agent links Jira ticket after creation)
pub async fn update(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateApprovalRequest>,
) -> AppResult<Json<Approval>> {
    let approval = if let Some(jira_key) = &req.jira_key {
        services::approval::update_jira_key(&state.pool, &auth_user, id, jira_key).await?
    } else {
        return Err(crate::error::AppError::BadRequest("No fields to update".to_string()));
    };
    Ok(Json(approval))
}

/// POST /api/approvals/:id/approve
pub async fn approve(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Approval>> {
    let approval = services::approval::approve(&state.pool, &auth_user, id, &state.notification_tx).await?;
    Ok(Json(approval))
}

/// POST /api/approvals/:id/mark — admin marks executed approval as success/failure
pub async fn mark_result(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<MarkResultRequest>,
) -> AppResult<Json<Approval>> {
    let approval =
        services::approval::mark_result(&state.pool, &auth_user, id, req.success, &state.notification_tx).await?;
    Ok(Json(approval))
}

/// GET /api/approvals/jira-url — returns Jira base URL for link rendering.
pub async fn jira_url(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let url = services::approval::get_jira_base_url(&state.pool, auth_user.tenant_id).await;
    Ok(Json(serde_json::json!({ "url": url })))
}

/// POST /api/approvals/:id/reject
pub async fn reject(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Approval>> {
    let approval = services::approval::reject(&state.pool, &auth_user, id, &state.notification_tx).await?;
    Ok(Json(approval))
}

/// POST /api/approvals/:id/withdraw — requester withdraws own pending approval
pub async fn withdraw(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<Approval>> {
    let approval = services::approval::withdraw(&state.pool, &auth_user, id, &state.notification_tx).await?;
    Ok(Json(approval))
}
