use axum::{
    Json,
    extract::{Path, State},
};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppResult;
use crate::middleware::auth::AuthUser;
use crate::models::provider::{
    AssignProvidersRequest, AssignTenantsRequest, CreateProviderRequest, Provider, ProviderTenantAssignment,
    ProviderTypeOption, ProviderWithDefault, SetDefaultProviderRequest, UpdateProviderRequest,
};
use crate::services;

/// GET /api/providers — list model cards visible to the current user
pub async fn list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<ProviderWithDefault>>> {
    let rows = services::provider::list(&state.pool, &auth_user).await?;
    Ok(Json(rows))
}

/// GET /api/providers/types — available provider types based on environment
pub async fn available_types(State(state): State<AppState>) -> AppResult<Json<Vec<ProviderTypeOption>>> {
    let types = services::provider::available_types(state.config.env.is_local());
    Ok(Json(types))
}

/// POST /api/providers — create a new model card (super_admin only)
pub async fn create(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<CreateProviderRequest>,
) -> AppResult<Json<Provider>> {
    let row = services::provider::create(&state.pool, &auth_user, req).await?;
    Ok(Json(row))
}

/// PUT /api/providers/:id — update a model card (super_admin only)
pub async fn update(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateProviderRequest>,
) -> AppResult<Json<Provider>> {
    let row = services::provider::update(&state.pool, &auth_user, id, req).await?;
    Ok(Json(row))
}

/// DELETE /api/providers/:id — delete a model card (super_admin only)
pub async fn delete(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    services::provider::delete(&state.pool, &auth_user, id).await?;
    Ok(Json(serde_json::json!({"message": "Provider deleted"})))
}

/// GET /api/providers/assignments — count tenant assignments per provider (super_admin)
pub async fn tenant_assignment_counts(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<(Uuid, i64)>>> {
    crate::services::common::require_super_admin(&auth_user, "view assignments")?;
    let counts = services::provider::count_tenant_assignments(&state.pool).await?;
    Ok(Json(counts))
}

// ─── Provider-centric tenant assignment ─────────────────────────────

/// GET /api/providers/:id/tenants — list tenants assigned to this provider
pub async fn list_provider_tenants(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(provider_id): Path<Uuid>,
) -> AppResult<Json<Vec<ProviderTenantAssignment>>> {
    let rows = services::provider::list_tenants_for_provider(&state.pool, &auth_user, provider_id).await?;
    Ok(Json(rows))
}

/// PUT /api/providers/:id/tenants — assign this provider to multiple tenants
pub async fn assign_provider_tenants(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(provider_id): Path<Uuid>,
    Json(req): Json<AssignTenantsRequest>,
) -> AppResult<Json<serde_json::Value>> {
    services::provider::assign_provider_to_tenants(&state.pool, &auth_user, provider_id, req.tenant_ids).await?;
    Ok(Json(serde_json::json!({"message": "Provider assigned to tenants"})))
}

// ─── Tenant provider routes ────────────────────────────────────────

/// GET /api/tenants/:id/providers — list providers assigned to a tenant
pub async fn list_tenant_providers(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(tenant_id): Path<Uuid>,
) -> AppResult<Json<Vec<ProviderWithDefault>>> {
    let rows = services::provider::list_by_tenant(&state.pool, &auth_user, tenant_id).await?;
    Ok(Json(rows))
}

/// PUT /api/tenants/:id/providers — batch assign providers to a tenant
pub async fn assign_tenant_providers(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(tenant_id): Path<Uuid>,
    Json(req): Json<AssignProvidersRequest>,
) -> AppResult<Json<serde_json::Value>> {
    services::provider::assign_to_tenant(&state.pool, &auth_user, tenant_id, req.provider_ids).await?;
    Ok(Json(serde_json::json!({"message": "Providers assigned"})))
}

/// PUT /api/tenants/:id/providers/default — set the default provider for a tenant
pub async fn set_tenant_default_provider(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(tenant_id): Path<Uuid>,
    Json(req): Json<SetDefaultProviderRequest>,
) -> AppResult<Json<serde_json::Value>> {
    services::provider::set_tenant_default(&state.pool, &auth_user, tenant_id, req.provider_id).await?;
    Ok(Json(serde_json::json!({"message": "Default provider set"})))
}
