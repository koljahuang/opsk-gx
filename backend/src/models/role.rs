use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::FromRow;

#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Role {
    pub name: String,
    pub label: String,
    pub description: String,
    pub permissions: serde_json::Value, // JSON array of permission strings
    pub is_system: bool,
    pub created_at: DateTime<Utc>,
}
