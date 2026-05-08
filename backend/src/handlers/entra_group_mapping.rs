use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppResult;
use crate::middleware::auth::AuthUser;
use crate::models::entra_group_mapping::{
    CreateEntraGroupMappingRequest, EntraGroupMapping, UpdateEntraGroupMappingRequest,
};
use crate::services;

/// GET /api/entra-group-mappings
pub async fn list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<EntraGroupMapping>>> {
    let mappings = services::entra_group_mapping::list(&state.pool, &auth_user).await?;
    Ok(Json(mappings))
}

/// POST /api/entra-group-mappings
pub async fn create(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<CreateEntraGroupMappingRequest>,
) -> AppResult<Json<EntraGroupMapping>> {
    let mapping = services::entra_group_mapping::create(&state.pool, &auth_user, req).await?;
    Ok(Json(mapping))
}

/// PUT /api/entra-group-mappings/:id
pub async fn update(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateEntraGroupMappingRequest>,
) -> AppResult<Json<EntraGroupMapping>> {
    let mapping = services::entra_group_mapping::update(&state.pool, &auth_user, id, req).await?;
    Ok(Json(mapping))
}

/// DELETE /api/entra-group-mappings/:id
pub async fn delete(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    services::entra_group_mapping::delete(&state.pool, &auth_user, id).await?;
    Ok(Json(serde_json::json!({"message": "Group mapping deleted"})))
}
