use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;
use uuid::Uuid;

use crate::config::MicrosoftOAuthConfig;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct EntraIdConnection {
    pub id: Uuid,
    pub name: String,
    pub entra_tenant_id: String,
    pub client_id: String,
    #[serde(skip_serializing)]
    pub client_secret: String,
    pub tenant_id: Uuid,
    pub auto_provision: bool,
    pub default_role: String,
    pub enabled: bool,
    pub allowed_domains: Vec<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl EntraIdConnection {
    /// Build a MicrosoftOAuthConfig from this connection's credentials.
    pub fn to_oauth_config(&self) -> MicrosoftOAuthConfig {
        MicrosoftOAuthConfig {
            client_id: self.client_id.clone(),
            client_secret: self.client_secret.clone(),
            tenant_id: self.entra_tenant_id.clone(),
            redirect_uris: vec![],
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateEntraIdConnectionRequest {
    pub name: String,
    pub entra_tenant_id: String,
    pub client_id: String,
    pub client_secret: String,
    pub tenant_id: Uuid,
    pub auto_provision: Option<bool>,
    pub default_role: Option<String>,
    pub allowed_domains: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateEntraIdConnectionRequest {
    pub name: Option<String>,
    pub entra_tenant_id: Option<String>,
    pub client_id: Option<String>,
    pub client_secret: Option<String>,
    pub tenant_id: Option<Uuid>,
    pub auto_provision: Option<bool>,
    pub default_role: Option<String>,
    pub enabled: Option<bool>,
    pub allowed_domains: Option<Vec<String>>,
}
