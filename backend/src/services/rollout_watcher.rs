//! Rollout status watcher — uses K8s Watch API (event-driven) to detect Argo Rollout
//! changes in real-time across all clusters and record them as deployment_events.
//!
//! Architecture:
//!   Manager loop (every 30s) → discovers active clusters from DB
//!     → spawns per-cluster watcher tasks for new clusters
//!     → aborts watcher tasks for removed clusters
//!   Each watcher task → kube::runtime::watcher() stream on Rollout CRD
//!     → compares snapshot on each Applied event → fires notification on change

use std::collections::{HashMap, HashSet};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;
use uuid::Uuid;

use futures::TryStreamExt;
use kube::api::{Api, DynamicObject};
use kube::runtime::WatchStreamExt;
use kube::runtime::watcher;
use sqlx::PgPool;

use crate::models::cluster::Cluster;
use crate::models::notification::Notification;
use crate::services::k8s::build_k8s_client;
use crate::services::rollout::{record_event, rollout_api_resource};

/// Snapshot of a rollout's state for change detection.
#[derive(Debug, Clone, PartialEq)]
struct RolloutSnapshot {
    phase: String,
    current_step: i64,
    replicas: i64,
    ready_replicas: i64,
    image: String,
}

/// Extract a RolloutSnapshot from a DynamicObject.
fn extract_snapshot(obj: &DynamicObject) -> RolloutSnapshot {
    let status = obj.data.get("status");
    let spec = obj.data.get("spec");

    let phase = status
        .and_then(|s| s.get("phase"))
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown")
        .to_string();

    let current_step = status
        .and_then(|s| s.get("currentStepIndex"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let replicas = status
        .and_then(|s| s.get("replicas"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let ready_replicas = status
        .and_then(|s| s.get("readyReplicas"))
        .and_then(|v| v.as_i64())
        .unwrap_or(0);

    let image = spec
        .and_then(|s| s.get("template"))
        .and_then(|t| t.get("spec"))
        .and_then(|s| s.get("containers"))
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|c| c.get("image"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    RolloutSnapshot {
        phase,
        current_step,
        replicas,
        ready_replicas,
        image,
    }
}

/// Compute human-readable change descriptions between two snapshots.
fn compute_changes(old: &RolloutSnapshot, new: &RolloutSnapshot) -> (Vec<String>, &'static str) {
    let mut changes = Vec::new();
    if old.phase != new.phase {
        changes.push(format!("phase: {} → {}", old.phase, new.phase));
    }
    if old.current_step != new.current_step {
        changes.push(format!("step: {} → {}", old.current_step, new.current_step));
    }
    if old.image != new.image {
        changes.push(format!("image: {} → {}", old.image, new.image));
    }
    if old.replicas != new.replicas || old.ready_replicas != new.ready_replicas {
        changes.push(format!(
            "replicas: {}/{} → {}/{}",
            old.ready_replicas, old.replicas, new.ready_replicas, new.replicas
        ));
    }

    let action = if old.phase != new.phase {
        "phase_change"
    } else if old.current_step != new.current_step {
        "step_advance"
    } else if old.image != new.image {
        "image_update"
    } else {
        "replica_change"
    };

    (changes, action)
}

/// Entry point: manages per-cluster watcher tasks. Runs forever.
pub async fn run_rollout_watcher(pool: PgPool, notification_tx: broadcast::Sender<Notification>) {
    tracing::info!("Rollout watcher started (event-driven, K8s Watch API)");

    // cluster_id → (JoinHandle, AbortHandle)
    let mut watchers: HashMap<Uuid, JoinHandle<()>> = HashMap::new();

    // Manager loop: discover clusters every 30s, spawn/remove watchers
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    // Skip first tick — let the server warm up
    interval.tick().await;

    loop {
        interval.tick().await;

        let clusters =
            match sqlx::query_as::<_, Cluster>("SELECT * FROM clusters WHERE status = 'active' AND cloud = 'aws'")
                .fetch_all(&pool)
                .await
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("Rollout watcher: DB error listing clusters: {e}");
                    continue;
                }
            };

        let active_ids: HashSet<Uuid> = clusters.iter().map(|c| c.id).collect();

        // Remove watchers for clusters that no longer exist or became inactive
        watchers.retain(|id, handle| {
            if !active_ids.contains(id) {
                tracing::info!("Rollout watcher: stopping watcher for removed cluster {id}");
                handle.abort();
                false
            } else if handle.is_finished() {
                // Watcher task died (error/panic) — remove so it gets re-spawned next cycle
                tracing::warn!("Rollout watcher: watcher for cluster {id} exited, will respawn");
                false
            } else {
                true
            }
        });

        // Spawn watchers for new clusters
        for cluster in &clusters {
            if watchers.contains_key(&cluster.id) {
                continue;
            }

            tracing::info!(
                "Rollout watcher: spawning watcher for cluster {} ({})",
                cluster.name,
                cluster.id
            );

            let cluster_id = cluster.id;
            let pool = pool.clone();
            let tx = notification_tx.clone();
            let cluster = cluster.clone();

            let handle = tokio::spawn(async move {
                run_cluster_watcher(pool, cluster, tx).await;
            });

            watchers.insert(cluster_id, handle);
        }

        if !watchers.is_empty() {
            tracing::debug!("Rollout watcher: {} active cluster watcher(s)", watchers.len());
        }
    }
}

/// Watch a single cluster's Rollout CRDs via K8s Watch API.
/// Runs until cancelled (abort) or unrecoverable error.
/// On transient errors, retries with backoff.
async fn run_cluster_watcher(pool: PgPool, cluster: Cluster, notification_tx: broadcast::Sender<Notification>) {
    let mut retry_delay = std::time::Duration::from_secs(5);
    let max_delay = std::time::Duration::from_secs(300);

    loop {
        match watch_cluster_rollouts(&pool, &cluster, &notification_tx).await {
            Ok(()) => {
                // Stream ended cleanly (shouldn't normally happen with watcher)
                tracing::warn!(
                    "Rollout watcher: stream ended for cluster {} ({}), restarting...",
                    cluster.name,
                    cluster.id
                );
                retry_delay = std::time::Duration::from_secs(5);
            }
            Err(e) => {
                tracing::warn!(
                    "Rollout watcher: error for cluster {} ({}): {e}, retrying in {retry_delay:?}",
                    cluster.name,
                    cluster.id,
                );
            }
        }

        tokio::time::sleep(retry_delay).await;
        retry_delay = (retry_delay * 2).min(max_delay);
    }
}

/// Core watch loop for one cluster. Returns Err on failure, Ok if stream ends.
async fn watch_cluster_rollouts(
    pool: &PgPool,
    cluster: &Cluster,
    notification_tx: &broadcast::Sender<Notification>,
) -> Result<(), String> {
    let client = build_k8s_client(pool, cluster)
        .await
        .map_err(|e| format!("k8s client: {e}"))?;

    let ar = rollout_api_resource();
    let api: Api<DynamicObject> = Api::all_with(client, &ar);

    // watcher::Config can be customized (e.g. timeout). Default is fine.
    let watch_config = watcher::Config::default();
    let stream = watcher(api, watch_config).default_backoff();
    let mut stream = std::pin::pin!(stream);

    let mut snapshots: HashMap<String, RolloutSnapshot> = HashMap::new();
    // Track whether initial list is complete — suppress notifications during init
    let mut init_done = false;

    tracing::debug!(
        "Rollout watcher: watch stream opened for cluster {} ({})",
        cluster.name,
        cluster.id
    );

    while let Some(event) = stream
        .try_next()
        .await
        .map_err(|e| format!("watch stream error: {e}"))?
    {
        match event {
            watcher::Event::Apply(obj) => {
                let name = obj.metadata.name.as_deref().unwrap_or("unknown");
                let namespace = obj.metadata.namespace.as_deref().unwrap_or("default");
                let key = format!("{}/{}", namespace, name);

                let new_snap = extract_snapshot(&obj);

                if !init_done {
                    // Initial list phase — just record baseline, no notifications
                    snapshots.insert(key, new_snap);
                    continue;
                }

                if let Some(old_snap) = snapshots.get(&key)
                    && *old_snap != new_snap
                {
                    let (changes, action) = compute_changes(old_snap, &new_snap);

                    tracing::info!(
                        "Rollout change: {}/{} on {} — {}",
                        namespace,
                        name,
                        cluster.name,
                        changes.join(", ")
                    );

                    let detail = serde_json::json!({
                        "changes": changes,
                        "phase": new_snap.phase,
                        "step": new_snap.current_step,
                        "replicas": new_snap.replicas,
                        "ready_replicas": new_snap.ready_replicas,
                        "image": new_snap.image,
                    });

                    record_event(
                        pool,
                        cluster.id,
                        namespace,
                        name,
                        action,
                        detail,
                        None,
                        cluster.tenant_id,
                    )
                    .await;

                    // Notify tenant admins
                    let notify_title = format!("{}/{} — {}", namespace, name, action.replace('_', " "));
                    let notify_desc = format!("Cluster: {} | {}", cluster.name, changes.join(", "));
                    let payload = serde_json::json!({
                        "cluster_id": cluster.id.to_string(),
                        "cluster_name": &cluster.name,
                        "namespace": namespace,
                        "rollout_name": name,
                        "action": action,
                        "phase": &new_snap.phase,
                    });
                    crate::services::notification::notify_tenant_admins(
                        pool,
                        cluster.tenant_id,
                        "deployment_change",
                        &notify_title,
                        &notify_desc,
                        payload,
                        None,
                        None,
                        Some(notification_tx),
                    )
                    .await;
                }
                // else: first time seeing this rollout post-init — record baseline

                snapshots.insert(key, new_snap);
            }
            watcher::Event::Delete(obj) => {
                let name = obj.metadata.name.as_deref().unwrap_or("unknown");
                let namespace = obj.metadata.namespace.as_deref().unwrap_or("default");
                let key = format!("{}/{}", namespace, name);

                if snapshots.remove(&key).is_some() && init_done {
                    tracing::info!("Rollout deleted: {}/{} on {}", namespace, name, cluster.name);

                    let detail = serde_json::json!({ "action": "deleted" });
                    record_event(
                        pool,
                        cluster.id,
                        namespace,
                        name,
                        "deleted",
                        detail,
                        None,
                        cluster.tenant_id,
                    )
                    .await;

                    let payload = serde_json::json!({
                        "cluster_id": cluster.id.to_string(),
                        "cluster_name": &cluster.name,
                        "namespace": namespace,
                        "rollout_name": name,
                        "action": "deleted",
                    });
                    crate::services::notification::notify_tenant_admins(
                        pool,
                        cluster.tenant_id,
                        "deployment_change",
                        &format!("{}/{} — deleted", namespace, name),
                        &format!("Cluster: {}", cluster.name),
                        payload,
                        None,
                        None,
                        Some(notification_tx),
                    )
                    .await;
                }
            }
            watcher::Event::Init => {
                // Initial list starting — clear any stale snapshots
                snapshots.clear();
                init_done = false;
                tracing::debug!("Rollout watcher: initial list started for cluster {}", cluster.name);
            }
            watcher::Event::InitApply(obj) => {
                // Initial list item — record baseline without notification
                let name = obj.metadata.name.as_deref().unwrap_or("unknown");
                let namespace = obj.metadata.namespace.as_deref().unwrap_or("default");
                let key = format!("{}/{}", namespace, name);
                snapshots.insert(key, extract_snapshot(&obj));
            }
            watcher::Event::InitDone => {
                init_done = true;
                tracing::debug!(
                    "Rollout watcher: initial list done for cluster {} ({} rollouts)",
                    cluster.name,
                    snapshots.len()
                );
            }
        }
    }

    Ok(())
}
