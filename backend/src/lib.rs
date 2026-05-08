pub mod config;
pub mod db;
pub mod error;
pub mod handlers;
pub mod middleware;
pub mod models;
pub mod services;

pub use error::{AppError, AppResult};

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::RwLock;

/// Role permission cache: role_name → set of permission strings.
/// Loaded from `roles` table at startup, refreshed when roles change.
pub type RoleCache = Arc<RwLock<HashMap<String, HashSet<String>>>>;

/// Shared application state — accessible by all handlers via `State<AppState>`
#[derive(Clone)]
pub struct AppState {
    pub pool: sqlx::PgPool,
    pub config: config::AppConfig,
    pub rca_registry: Arc<services::rca::RcaRegistry>,
    pub topology_cache: Arc<handlers::topology::TopologyCache>,
    pub notification_tx: tokio::sync::broadcast::Sender<models::notification::Notification>,
    pub role_cache: RoleCache,
    /// Runtime-togglable auto-RCA flag (initialized from config, toggleable via API).
    pub auto_rca_enabled: Arc<AtomicBool>,
}

/// Load all roles from DB into the permission cache.
pub async fn load_role_cache(pool: &sqlx::PgPool) -> HashMap<String, HashSet<String>> {
    let rows: Vec<(String, serde_json::Value)> = sqlx::query_as("SELECT name, permissions FROM roles")
        .fetch_all(pool)
        .await
        .unwrap_or_default();

    let mut cache = HashMap::new();
    for (name, perms) in rows {
        let set: HashSet<String> = perms
            .as_array()
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();
        cache.insert(name, set);
    }
    cache
}
