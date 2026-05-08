use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Provider {
    pub id: Uuid,
    pub name: String,
    pub provider_type: String,
    pub config: serde_json::Value,
    pub secret_arn: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// Junction table: which providers are assigned to which tenants
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct TenantProvider {
    pub tenant_id: Uuid,
    pub provider_id: Uuid,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

/// Provider with its tenant-level is_default flag (for list_by_tenant)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderWithDefault {
    #[serde(flatten)]
    pub provider: Provider,
    pub is_default: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateProviderRequest {
    pub name: String,
    pub provider_type: String,
    pub config: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub struct UpdateProviderRequest {
    pub name: Option<String>,
    pub provider_type: Option<String>,
    pub config: Option<serde_json::Value>,
}

/// Batch assign providers to a tenant (replaces existing assignments)
#[derive(Debug, Deserialize)]
pub struct AssignProvidersRequest {
    pub provider_ids: Vec<Uuid>,
}

/// Set the default provider for a tenant
#[derive(Debug, Deserialize)]
pub struct SetDefaultProviderRequest {
    pub provider_id: Uuid,
}

/// Batch assign tenants to a provider (provider-centric assignment)
#[derive(Debug, Deserialize)]
pub struct AssignTenantsRequest {
    pub tenant_ids: Vec<Uuid>,
}

/// Tenant info with is_default flag (returned from provider-centric tenant list)
#[derive(Debug, Clone, Serialize, FromRow)]
pub struct ProviderTenantAssignment {
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub is_default: bool,
}

/// Available provider type for frontend dropdown
#[derive(Debug, Serialize)]
pub struct ProviderTypeOption {
    pub value: String,
    pub label: String,
}
