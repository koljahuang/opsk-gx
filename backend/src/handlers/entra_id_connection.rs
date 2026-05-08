use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppResult;
use crate::middleware::auth::AuthUser;
use crate::models::entra_id_connection::{
    CreateEntraIdConnectionRequest, EntraIdConnection, UpdateEntraIdConnectionRequest,
};
use crate::services;

/// GET /api/entra-id-connections
pub async fn list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<EntraIdConnection>>> {
    let connections = services::entra_id_connection::list(&state.pool, &auth_user).await?;
    Ok(Json(connections))
}

/// GET /api/entra-id-connections/:id
pub async fn get(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<EntraIdConnection>> {
    let connection = services::entra_id_connection::get(&state.pool, &auth_user, id).await?;
    Ok(Json(connection))
}

/// POST /api/entra-id-connections
pub async fn create(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<CreateEntraIdConnectionRequest>,
) -> AppResult<Json<EntraIdConnection>> {
    let connection = services::entra_id_connection::create(&state.pool, &auth_user, req).await?;
    Ok(Json(connection))
}

/// PUT /api/entra-id-connections/:id
pub async fn update(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateEntraIdConnectionRequest>,
) -> AppResult<Json<EntraIdConnection>> {
    let connection = services::entra_id_connection::update(&state.pool, &auth_user, id, req).await?;
    Ok(Json(connection))
}

/// DELETE /api/entra-id-connections/:id
pub async fn delete(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    services::entra_id_connection::delete(&state.pool, &auth_user, id).await?;
    Ok(Json(serde_json::json!({"message": "Connection deleted"})))
}
