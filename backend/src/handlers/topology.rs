use axum::{Json, extract::State};
use k8s_openapi::api::apps::v1::{Deployment, ReplicaSet};
use k8s_openapi::api::core::v1::{Node, Pod, Service};
use k8s_openapi::api::networking::v1::Ingress;
use kube::api::{Api, ListParams};
use moka::future::Cache;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::cluster::Cluster;
use crate::services::k8s::build_k8s_client;

// ─── Cache ──────────────────────────────────────────────────────────────────

/// Per-cluster topology cache with TTL and bounded size.
pub struct TopologyCache {
    data: Cache<String, (Vec<TopoNode>, Vec<TopoEdge>)>,
}

impl Default for TopologyCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TopologyCache {
    pub fn new() -> Self {
        Self {
            data: Cache::builder()
                .max_capacity(100)
                .time_to_live(Duration::from_secs(3600))
                .build(),
        }
    }

    async fn get_cluster(&self, cluster_id: &str) -> Option<(Vec<TopoNode>, Vec<TopoEdge>)> {
        self.data.get(cluster_id).await
    }

    async fn set_cluster(&self, cluster_id: String, nodes: Vec<TopoNode>, edges: Vec<TopoEdge>) {
        self.data.insert(cluster_id, (nodes, edges)).await;
    }

    async fn invalidate(&self) {
        self.data.invalidate_all();
    }

    async fn invalidate_cluster(&self, cluster_id: &str) {
        self.data.invalidate(cluster_id).await;
    }
}

// ─── Query params ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct TopologyQuery {
    pub force_refresh: Option<bool>,
    /// Filter to a single cluster by ID. When set, only that cluster's topology is returned.
    pub cluster_id: Option<String>,
}

// ─── Response types ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
pub struct TopologyResponse {
    pub nodes: Vec<TopoNode>,
    pub edges: Vec<TopoEdge>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopoNode {
    pub id: String,
    pub label: String,
    pub subtitle: Option<String>,
    pub kind: String, // "ingress" | "service" | "deployment" | "rollout" | "pod" | "node"
    pub namespace: String,
    pub cluster: String,
    pub cluster_id: String,
    pub status: String, // "healthy" | "warning" | "critical" | "unknown"
    pub replicas: Option<String>,
    /// Extra metadata for rich node display
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ip: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub containers: Option<i32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TopoEdge {
    pub id: String,
    pub source: String,
    pub target: String,
    pub label: Option<String>,
}

// ─── GET /api/topology ──────────────────────────────────────────────────────

/// Build a complete 5-tier K8s topology graph from all accessible clusters.
/// Discovers: Ingress → Service → Deployment/Rollout → Pod → Node relationships.
pub async fn get_topology(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    axum::extract::Query(params): axum::extract::Query<TopologyQuery>,
) -> AppResult<Json<TopologyResponse>> {
    let force_refresh = params.force_refresh.unwrap_or(false);
    let filter_cluster_id = params.cluster_id.as_deref();

    // Get clusters the user can access (optionally filtered by cluster_id)
    let clusters: Vec<Cluster> = if let Some(cid) = filter_cluster_id {
        // Single-cluster mode: fetch only the requested cluster
        let cluster_uuid: uuid::Uuid = cid
            .parse()
            .map_err(|_| AppError::BadRequest("Invalid cluster_id".to_string()))?;

        if force_refresh {
            state.topology_cache.invalidate_cluster(cid).await;
        }

        let cluster: Option<Cluster> = if auth_user.is_super_admin() {
            sqlx::query_as("SELECT * FROM clusters WHERE id = $1 AND UPPER(status) = 'ACTIVE'")
                .bind(cluster_uuid)
                .fetch_optional(&state.pool)
                .await?
        } else {
            sqlx::query_as("SELECT * FROM clusters WHERE id = $1 AND UPPER(status) = 'ACTIVE' AND tenant_id IS NOT DISTINCT FROM $2")
                .bind(cluster_uuid)
                .bind(auth_user.tenant_id)
                .fetch_optional(&state.pool)
                .await?
        };

        match cluster {
            Some(c) => vec![c],
            None => return Err(AppError::NotFound("Cluster not found".to_string())),
        }
    } else {
        // All-clusters mode
        if force_refresh {
            state.topology_cache.invalidate().await;
        }

        if auth_user.is_super_admin() {
            sqlx::query_as("SELECT * FROM clusters WHERE UPPER(status) = 'ACTIVE'")
                .fetch_all(&state.pool)
                .await?
        } else {
            sqlx::query_as(
                "SELECT * FROM clusters WHERE UPPER(status) = 'ACTIVE' AND tenant_id IS NOT DISTINCT FROM $1",
            )
            .bind(auth_user.tenant_id)
            .fetch_all(&state.pool)
            .await?
        }
    };

    let mut nodes: Vec<TopoNode> = Vec::new();
    let mut edges: Vec<TopoEdge> = Vec::new();

    // Per-cluster: check cache first, fetch on miss — runs concurrently
    let handles: Vec<_> = clusters
        .into_iter()
        .map(|cluster| {
            let pool = state.pool.clone();
            let cache = state.topology_cache.clone();
            let cluster_id = cluster.id.to_string();
            tokio::spawn(async move {
                if !force_refresh && let Some((n, e)) = cache.get_cluster(&cluster_id).await {
                    return (n, e);
                }
                match build_topology_for_cluster(&pool, &cluster).await {
                    Ok((n, e)) => {
                        cache.set_cluster(cluster_id, n.clone(), e.clone()).await;
                        (n, e)
                    }
                    Err(err) => {
                        tracing::warn!("Topology fetch failed for {}: {err}", cluster.name);
                        (vec![], vec![])
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        if let Ok((n, e)) = handle.await {
            nodes.extend(n);
            edges.extend(e);
        }
    }

    Ok(Json(TopologyResponse { nodes, edges }))
}

// ─── Helpers ───────────────────────────────────────────────────────────────

/// Context for building topo nodes within a single cluster — avoids repeating
/// cluster_name and cluster_id on every call.
struct ClusterCtx {
    cluster_name: String,
    cluster_id: String,
}

impl ClusterCtx {
    fn node(
        &self,
        id: String,
        label: String,
        subtitle: Option<String>,
        kind: &str,
        namespace: &str,
        status: &str,
    ) -> TopoNode {
        TopoNode {
            id,
            label,
            subtitle,
            kind: kind.to_string(),
            namespace: namespace.to_string(),
            cluster: self.cluster_name.clone(),
            cluster_id: self.cluster_id.clone(),
            status: status.to_string(),
            replicas: None,
            node_name: None,
            cpu: None,
            memory: None,
            ip: None,
            containers: None,
        }
    }
}

/// Excluded system namespaces
const EXCLUDE_NS: &[&str] = &[
    "kube-system",
    "kube-public",
    "kube-node-lease",
    "argo-rollouts",
    "argocd",
    "cert-manager",
    "external-secrets",
    "monitoring",
    "grafana",
    "loki",
    "mimir",
    "tempo",
    "ingress-nginx",
    "karpenter",
];

fn is_excluded_ns(ns: &str) -> bool {
    EXCLUDE_NS.contains(&ns)
}

/// Build topology nodes + edges for a single cluster.
async fn build_topology_for_cluster(
    pool: &sqlx::PgPool,
    cluster: &Cluster,
) -> AppResult<(Vec<TopoNode>, Vec<TopoEdge>)> {
    let client = build_k8s_client(pool, cluster).await?;
    let cluster_id = cluster.id.to_string();
    let ctx = ClusterCtx {
        cluster_name: cluster.name.clone(),
        cluster_id: cluster_id.clone(),
    };

    let mut nodes = Vec::new();
    let mut edges = Vec::new();

    // ─── Fetch all resources concurrently ───────────────────────
    let deploy_api: Api<Deployment> = Api::all(client.clone());
    let rs_api: Api<ReplicaSet> = Api::all(client.clone());
    let svc_api: Api<Service> = Api::all(client.clone());
    let ingress_api: Api<Ingress> = Api::all(client.clone());
    let pod_api: Api<Pod> = Api::all(client.clone());
    let node_api: Api<Node> = Api::all(client.clone());

    let lp = ListParams::default();
    let (deploys, replica_sets, services, ingresses, pods, k8s_nodes) = tokio::try_join!(
        async {
            deploy_api
                .list(&lp)
                .await
                .map_err(|e| AppError::Kubernetes(format!("List deployments: {e}")))
        },
        async {
            rs_api
                .list(&lp)
                .await
                .map_err(|e| AppError::Kubernetes(format!("List replicasets: {e}")))
        },
        async {
            svc_api
                .list(&lp)
                .await
                .map_err(|e| AppError::Kubernetes(format!("List services: {e}")))
        },
        async {
            ingress_api
                .list(&lp)
                .await
                .map_err(|e| AppError::Kubernetes(format!("List ingresses: {e}")))
        },
        async {
            pod_api
                .list(&lp)
                .await
                .map_err(|e| AppError::Kubernetes(format!("List pods: {e}")))
        },
        async {
            node_api
                .list(&lp)
                .await
                .map_err(|e| AppError::Kubernetes(format!("List nodes: {e}")))
        },
    )?;

    // Fetch Argo Rollouts separately (CRD may not exist)
    let rollout_items = {
        let ar = kube::discovery::ApiResource {
            group: "argoproj.io".to_string(),
            version: "v1alpha1".to_string(),
            api_version: "argoproj.io/v1alpha1".to_string(),
            kind: "Rollout".to_string(),
            plural: "rollouts".to_string(),
        };
        let rollout_api: Api<kube::api::DynamicObject> = Api::all_with(client.clone(), &ar);
        rollout_api.list(&lp).await.ok().map(|l| l.items).unwrap_or_default()
    };

    // ─── Build ReplicaSet → owner maps (Deployment + Rollout) ───
    let mut rs_to_deploy: HashMap<(String, String), String> = HashMap::new();
    let mut rs_to_rollout: HashMap<(String, String), String> = HashMap::new();
    for rs in &replica_sets.items {
        let ns = rs.metadata.namespace.as_deref().unwrap_or("default");
        let rs_name = rs.metadata.name.as_deref().unwrap_or("unknown");
        if let Some(owners) = &rs.metadata.owner_references {
            for owner in owners {
                let key = (ns.to_string(), rs_name.to_string());
                match owner.kind.as_str() {
                    "Deployment" => {
                        rs_to_deploy.insert(key, owner.name.clone());
                    }
                    "Rollout" => {
                        rs_to_rollout.insert(key, owner.name.clone());
                    }
                    _ => {}
                }
            }
        }
    }

    // ─── Nodes ──────────────────────────────────────────────────
    for n in &k8s_nodes.items {
        let name = n.metadata.name.as_deref().unwrap_or("unknown");
        let status_obj = n.status.as_ref();

        // Determine health from conditions
        let health = status_obj
            .and_then(|s| s.conditions.as_ref())
            .and_then(|conds| conds.iter().find(|c| c.type_ == "Ready"))
            .map(|c| if c.status == "True" { "healthy" } else { "critical" })
            .unwrap_or("unknown");

        // Extract capacity
        let capacity = status_obj.and_then(|s| s.capacity.as_ref());
        let cpu = capacity.and_then(|c| c.get("cpu")).map(|q| q.0.clone());
        let memory = capacity.and_then(|c| c.get("memory")).map(|q| q.0.clone());

        // Extract instance type and zone from labels
        let labels = n.metadata.labels.as_ref();
        let instance_type = labels.and_then(|l| l.get("node.kubernetes.io/instance-type")).cloned();
        let zone = labels.and_then(|l| l.get("topology.kubernetes.io/zone")).cloned();

        let subtitle = match (instance_type, zone) {
            (Some(it), Some(z)) => Some(format!("{it} · {z}")),
            (Some(it), None) => Some(it),
            (None, Some(z)) => Some(z),
            _ => None,
        };

        let mut node = ctx.node(
            format!("{cluster_id}/node/{name}"),
            name.to_string(),
            subtitle,
            "node",
            "_cluster_",
            health,
        );
        node.cpu = cpu;
        node.memory = memory;
        nodes.push(node);
    }

    // ─── Deployments ────────────────────────────────────────────
    for d in &deploys.items {
        let ns = d.metadata.namespace.as_deref().unwrap_or("default");
        if is_excluded_ns(ns) {
            continue;
        }

        let name = d.metadata.name.as_deref().unwrap_or("unknown");
        let spec = d.spec.as_ref();
        let status = d.status.as_ref();

        let desired = spec.and_then(|s| s.replicas).unwrap_or(1);
        let ready = status.and_then(|s| s.ready_replicas).unwrap_or(0);
        let available = status.and_then(|s| s.available_replicas).unwrap_or(0);

        let health = if available >= desired {
            "healthy"
        } else if ready > 0 {
            "warning"
        } else {
            "critical"
        };

        let mut node = ctx.node(
            format!("{cluster_id}/deploy/{ns}/{name}"),
            name.to_string(),
            Some(format!("{ready}/{desired} ready")),
            "deployment",
            ns,
            health,
        );
        node.replicas = Some(format!("{ready}/{desired}"));
        nodes.push(node);
    }

    // ─── Argo Rollouts ──────────────────────────────────────────
    for obj in &rollout_items {
        let ns = obj.metadata.namespace.as_deref().unwrap_or("default");
        if is_excluded_ns(ns) {
            continue;
        }

        let name = obj.metadata.name.as_deref().unwrap_or("unknown");
        let raw = serde_json::to_value(obj).unwrap_or_default();
        let phase = raw
            .pointer("/status/phase")
            .and_then(|v| v.as_str())
            .unwrap_or("Unknown");
        let desired = raw.pointer("/spec/replicas").and_then(|v| v.as_i64()).unwrap_or(1);
        let ready = raw
            .pointer("/status/readyReplicas")
            .and_then(|v| v.as_i64())
            .unwrap_or(0);

        let health = match phase {
            "Healthy" => "healthy",
            "Progressing" | "Paused" => "warning",
            "Degraded" => "critical",
            _ => "unknown",
        };

        let strategy = if raw.pointer("/spec/strategy/canary").is_some() {
            "canary"
        } else if raw.pointer("/spec/strategy/blueGreen").is_some() {
            "blueGreen"
        } else {
            "unknown"
        };

        let mut node = ctx.node(
            format!("{cluster_id}/rollout/{ns}/{name}"),
            name.to_string(),
            Some(format!("{phase} · {strategy}")),
            "rollout",
            ns,
            health,
        );
        node.replicas = Some(format!("{ready}/{desired}"));
        nodes.push(node);
    }

    // ─── Pods ───────────────────────────────────────────────────
    for p in &pods.items {
        let ns = p.metadata.namespace.as_deref().unwrap_or("default");
        if is_excluded_ns(ns) {
            continue;
        }

        let name = p.metadata.name.as_deref().unwrap_or("unknown");
        let pod_status = p.status.as_ref();
        let phase = pod_status.and_then(|s| s.phase.as_deref()).unwrap_or("Unknown");
        let pod_node = p.spec.as_ref().and_then(|s| s.node_name.clone());
        let pod_ip = pod_status.and_then(|s| s.pod_ip.clone());

        // Check for crash loops
        let has_crash = pod_status
            .and_then(|s| s.container_statuses.as_ref())
            .map(|cs| {
                cs.iter().any(|c| {
                    c.state.as_ref().is_some_and(|st| {
                        st.waiting
                            .as_ref()
                            .is_some_and(|w| w.reason.as_deref() == Some("CrashLoopBackOff"))
                    })
                })
            })
            .unwrap_or(false);

        let health = if has_crash {
            "critical"
        } else {
            match phase {
                "Running" => "healthy",
                "Succeeded" => "healthy",
                "Pending" => "warning",
                "Failed" => "critical",
                _ => "unknown",
            }
        };

        let container_count = p.spec.as_ref().and_then(|s| s.containers.len().try_into().ok());

        let subtitle = match &pod_node {
            Some(nn) => Some(format!("{phase} · {nn}")),
            None => Some(phase.to_string()),
        };

        let mut node = ctx.node(
            format!("{cluster_id}/pod/{ns}/{name}"),
            name.to_string(),
            subtitle,
            "pod",
            ns,
            health,
        );
        node.node_name = pod_node.clone();
        node.ip = pod_ip;
        node.containers = container_count;
        nodes.push(node);

        // ── Edge: Pod → Node ──
        if let Some(nn) = &pod_node {
            edges.push(TopoEdge {
                id: format!("e-pod-{ns}-{name}-node-{nn}"),
                source: format!("{cluster_id}/pod/{ns}/{name}"),
                target: format!("{cluster_id}/node/{nn}"),
                label: None,
            });
        }

        // ── Edge: Deployment/Rollout → Pod (via ownerReferences chain) ──
        if let Some(owners) = &p.metadata.owner_references {
            for owner in owners {
                if owner.kind == "ReplicaSet" {
                    let rs_name = &owner.name;
                    // Check deploy ownership
                    if let Some(deploy_name) = rs_to_deploy.get(&(ns.to_string(), rs_name.clone())) {
                        edges.push(TopoEdge {
                            id: format!("e-deploy-{ns}-{deploy_name}-pod-{name}"),
                            source: format!("{cluster_id}/deploy/{ns}/{deploy_name}"),
                            target: format!("{cluster_id}/pod/{ns}/{name}"),
                            label: None,
                        });
                    }
                    // Check rollout ownership
                    if let Some(rollout_name) = rs_to_rollout.get(&(ns.to_string(), rs_name.clone())) {
                        edges.push(TopoEdge {
                            id: format!("e-rollout-{ns}-{rollout_name}-pod-{name}"),
                            source: format!("{cluster_id}/rollout/{ns}/{rollout_name}"),
                            target: format!("{cluster_id}/pod/{ns}/{name}"),
                            label: None,
                        });
                    }
                }
            }
        }
    }

    // ─── Services ───────────────────────────────────────────────
    let node_ids: std::collections::HashSet<String> = nodes.iter().map(|n| n.id.clone()).collect();

    for s in &services.items {
        let ns = s.metadata.namespace.as_deref().unwrap_or("default");
        if is_excluded_ns(ns) {
            continue;
        }

        let name = s.metadata.name.as_deref().unwrap_or("unknown");
        let spec = s.spec.as_ref();
        let svc_type = spec.and_then(|s| s.type_.as_deref()).unwrap_or("ClusterIP");
        let ports: Vec<String> = spec
            .and_then(|s| s.ports.as_ref())
            .map(|ps| ps.iter().map(|p| format!("{}", p.port)).collect())
            .unwrap_or_default();

        let svc_id = format!("{cluster_id}/svc/{ns}/{name}");

        nodes.push(ctx.node(
            svc_id.clone(),
            name.to_string(),
            Some(format!("{svc_type} · :{}", ports.join(","))),
            "service",
            ns,
            "healthy",
        ));

        // ── Link Service → Deployment/Rollout by matching selector to labels ──
        if let Some(selector) = spec.and_then(|s| s.selector.as_ref()) {
            for d in &deploys.items {
                let d_ns = d.metadata.namespace.as_deref().unwrap_or("default");
                if d_ns != ns {
                    continue;
                }

                let d_name = d.metadata.name.as_deref().unwrap_or("unknown");
                let tmpl_labels = d
                    .spec
                    .as_ref()
                    .and_then(|s| s.template.metadata.as_ref())
                    .and_then(|m| m.labels.as_ref());

                if let Some(labels) = tmpl_labels {
                    let matches = selector
                        .iter()
                        .all(|(k, v)| labels.get(k).map(|lv| lv == v).unwrap_or(false));
                    if matches {
                        edges.push(TopoEdge {
                            id: format!("e-{svc_id}-deploy-{d_name}"),
                            source: svc_id.clone(),
                            target: format!("{cluster_id}/deploy/{ns}/{d_name}"),
                            label: None,
                        });
                    }
                }
            }

            // Match rollouts by name convention
            let rollout_node_id = format!("{cluster_id}/rollout/{ns}/{name}");
            if node_ids.contains(rollout_node_id.as_str()) {
                edges.push(TopoEdge {
                    id: format!("e-{svc_id}-rollout-{name}"),
                    source: svc_id.clone(),
                    target: rollout_node_id,
                    label: None,
                });
            }
        }
    }

    // ─── Ingresses ──────────────────────────────────────────────
    for ing in &ingresses.items {
        let ns = ing.metadata.namespace.as_deref().unwrap_or("default");
        if is_excluded_ns(ns) {
            continue;
        }

        let name = ing.metadata.name.as_deref().unwrap_or("unknown");
        let spec = ing.spec.as_ref();

        let hosts: Vec<String> = spec
            .and_then(|s| s.rules.as_ref())
            .map(|rules| rules.iter().filter_map(|r| r.host.clone()).collect())
            .unwrap_or_default();

        let ing_id = format!("{cluster_id}/ingress/{ns}/{name}");

        nodes.push(ctx.node(
            ing_id.clone(),
            name.to_string(),
            if hosts.is_empty() { None } else { Some(hosts.join(", ")) },
            "ingress",
            ns,
            "healthy",
        ));

        // Link Ingress → Service
        if let Some(rules) = spec.and_then(|s| s.rules.as_ref()) {
            for rule in rules {
                if let Some(http) = &rule.http {
                    for path in &http.paths {
                        if let Some(backend_svc) = path.backend.service.as_ref() {
                            let target_svc = &backend_svc.name;
                            let target_id = format!("{cluster_id}/svc/{ns}/{target_svc}");
                            let path_str = path.path.as_deref().unwrap_or("/*");

                            edges.push(TopoEdge {
                                id: format!("e-{ing_id}-{target_svc}"),
                                source: ing_id.clone(),
                                target: target_id,
                                label: Some(path_str.to_string()),
                            });
                        }
                    }
                }
            }
        }
    }

    Ok((nodes, edges))
}
