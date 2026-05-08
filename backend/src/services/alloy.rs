//! Alloy (Grafana agent) configuration management.
//!
//! Reads telemetry_config from DB, generates Alloy River config,
//! patches the ConfigMap in k8s, and triggers a DaemonSet restart.

use sqlx::PgPool;

use crate::error::{AppError, AppResult};
use crate::models::telemetry::TelemetryConfig;

const CONFIGMAP_NAME: &str = "alloy";
const CONFIGMAP_KEY: &str = "config.alloy";
const NAMESPACE: &str = "monitoring";
const DAEMONSET_NAME: &str = "alloy";

/// Sync Alloy ConfigMap from the DB telemetry_config.
/// Called on create/update/delete of telemetry configs and once at startup.
pub async fn sync_alloy_config(pool: &PgPool) -> AppResult<()> {
    // 1. Load all enabled telemetry configs
    let configs: Vec<TelemetryConfig> =
        sqlx::query_as("SELECT * FROM telemetry_config WHERE enabled = true ORDER BY created_at ASC")
            .fetch_all(pool)
            .await?;

    // 2. Generate Alloy config — use first enabled config (single-backend for now)
    let alloy_config = if let Some(cfg) = configs.first() {
        generate_config(cfg)
    } else {
        generate_default_config()
    };

    // 3. Apply to k8s
    apply_configmap(&alloy_config).await?;

    // 4. Restart DaemonSet to pick up new config
    restart_daemonset().await?;

    tracing::info!("Alloy config synced ({} telemetry configs)", configs.len());
    Ok(())
}

/// Generate Alloy River config from a TelemetryConfig record.
fn generate_config(cfg: &TelemetryConfig) -> String {
    let c = &cfg.config;
    let mode = c.get("mode").and_then(|v| v.as_str()).unwrap_or("cloud");

    let mimir_url = c.get("mimir_endpoint_url").and_then(|v| v.as_str()).unwrap_or("");
    let mimir_user = c.get("mimir_user_id").and_then(|v| v.as_str()).unwrap_or("");
    let loki_url = c.get("loki_endpoint_url").and_then(|v| v.as_str()).unwrap_or("");
    let loki_user = c.get("loki_user_id").and_then(|v| v.as_str()).unwrap_or("");
    let tempo_url = c.get("tempo_endpoint_url").and_then(|v| v.as_str()).unwrap_or("");
    let tempo_user = c.get("tempo_user_id").and_then(|v| v.as_str()).unwrap_or("");
    let api_token = c.get("api_token").and_then(|v| v.as_str()).unwrap_or("");

    let is_cloud = mode == "cloud" && !api_token.is_empty();

    // Build Mimir remote_write auth block
    let mimir_auth = if is_cloud && !mimir_user.is_empty() {
        format!(
            r#"
    basic_auth {{
      username = "{mimir_user}"
      password = "{api_token}"
    }}"#
        )
    } else {
        String::new()
    };

    // Mimir endpoint — append /push only, user provides the base path
    // Cloud: user enters .../api/prom → we append /push → .../api/prom/push
    // Self-hosted: user enters http://mimir:9009 → we append /api/v1/push
    let mimir_push_url = if mimir_url.contains("/push") {
        mimir_url.to_string()
    } else if mimir_url.contains("/api/prom") || mimir_url.contains("/api/v1") {
        format!("{}/push", mimir_url.trim_end_matches('/'))
    } else {
        format!("{}/api/v1/push", mimir_url.trim_end_matches('/'))
    };

    // Loki push URL
    let loki_push_url = if loki_url.contains("/push") {
        loki_url.to_string()
    } else {
        format!("{}/loki/api/v1/push", loki_url.trim_end_matches('/'))
    };

    let loki_auth = if is_cloud && !loki_user.is_empty() {
        format!(
            r#"
    basic_auth {{
      username = "{loki_user}"
      password = "{api_token}"
    }}"#
        )
    } else {
        String::new()
    };

    // Tempo — both cloud and self-hosted use otelcol.exporter.otlp (gRPC).
    // Grafana Cloud Tempo accepts OTLP gRPC on port 443 (with TLS + BasicAuth).
    // It does NOT support OTLP HTTP — the /otlp path returns 404.
    let tempo_grpc_endpoint = if tempo_url.is_empty() {
        "tempo.monitoring.svc:4317".to_string()
    } else {
        // Strip scheme for gRPC endpoint; add :443 for Grafana Cloud if no port specified
        let stripped = tempo_url
            .trim_start_matches("http://")
            .trim_start_matches("https://")
            .trim_end_matches('/')
            .to_string();
        if stripped.contains("grafana.net") && !stripped.contains(':') {
            format!("{}:443", stripped)
        } else {
            stripped
        }
    };

    let tempo_block = if is_cloud && !tempo_user.is_empty() {
        format!(
            r#"
// ─── Traces: forward to Grafana Cloud Tempo (OTLP gRPC + BasicAuth) ───
otelcol.auth.basic "tempo" {{
  username = "{tempo_user}"
  password = "{api_token}"
}}

otelcol.exporter.otlp "tempo" {{
  client {{
    endpoint = "{tempo_grpc_endpoint}"
    auth     = otelcol.auth.basic.tempo.handler
  }}
}}"#
        )
    } else {
        // Self-hosted: gRPC direct, no TLS
        format!(
            r#"
// ─── Traces: forward to Tempo (gRPC) ───
otelcol.exporter.otlp "tempo" {{
  client {{
    endpoint = "{tempo_grpc_endpoint}"
    tls {{
      insecure = true
    }}
  }}
}}"#
        )
    };

    // Both cloud and self-hosted now use otelcol.exporter.otlp (gRPC)
    let traces_output = "otelcol.exporter.otlp.tempo.input";

    format!(
        r#"// ─── Kubernetes Discovery ───────────────────────────
discovery.kubernetes "pods" {{
  role = "pod"
}}

discovery.kubernetes "nodes" {{
  role = "node"
}}

// ─── Node metrics (kubelet /metrics endpoint) ───────
discovery.relabel "kubelet" {{
  targets = discovery.kubernetes.nodes.targets

  rule {{
    target_label  = "__address__"
    replacement   = "kubernetes.default.svc:443"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_node_name"]
    target_label  = "__metrics_path__"
    replacement   = "/api/v1/nodes/${{1}}/proxy/metrics"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_node_name"]
    target_label  = "node"
  }}
}}

prometheus.scrape "kubelet" {{
  targets      = discovery.relabel.kubelet.output
  scheme       = "https"
  scrape_interval = "30s"
  bearer_token_file = "/var/run/secrets/kubernetes.io/serviceaccount/token"
  tls_config {{
    ca_file              = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
    insecure_skip_verify = true
  }}
  forward_to = [prometheus.relabel.filter.receiver]
}}

// ─── Container metrics (cadvisor) ───────────────────
discovery.relabel "cadvisor" {{
  targets = discovery.kubernetes.nodes.targets

  rule {{
    target_label  = "__address__"
    replacement   = "kubernetes.default.svc:443"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_node_name"]
    target_label  = "__metrics_path__"
    replacement   = "/api/v1/nodes/${{1}}/proxy/metrics/cadvisor"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_node_name"]
    target_label  = "node"
  }}
}}

prometheus.scrape "cadvisor" {{
  targets      = discovery.relabel.cadvisor.output
  scheme       = "https"
  scrape_interval = "30s"
  bearer_token_file = "/var/run/secrets/kubernetes.io/serviceaccount/token"
  tls_config {{
    ca_file              = "/var/run/secrets/kubernetes.io/serviceaccount/ca.crt"
    insecure_skip_verify = true
  }}
  forward_to = [prometheus.relabel.filter.receiver]
}}

// ─── Application metrics (pods with prometheus.io annotations) ───
discovery.relabel "pod_metrics" {{
  targets = discovery.kubernetes.pods.targets

  rule {{
    source_labels = ["__meta_kubernetes_pod_annotation_prometheus_io_scrape"]
    regex         = "true"
    action        = "keep"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_pod_annotation_prometheus_io_scheme"]
    target_label  = "____scheme__"
    regex         = "(https?)"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_pod_annotation_prometheus_io_path"]
    target_label  = "__metrics_path__"
    regex         = "(.+)"
  }}
  rule {{
    source_labels = ["__address__", "__meta_kubernetes_pod_annotation_prometheus_io_port"]
    regex         = "([^:]+)(?:\\d+)?;(\\d+)"
    target_label  = "__address__"
    replacement   = "${{1}}:${{2}}"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_namespace"]
    target_label  = "namespace"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_pod_name"]
    target_label  = "pod"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_pod_container_name"]
    target_label  = "container"
  }}
  rule {{
    source_labels = ["__meta_kubernetes_pod_node_name"]
    target_label  = "node"
  }}
}}

prometheus.scrape "pods" {{
  targets      = discovery.relabel.pod_metrics.output
  scrape_interval = "30s"
  forward_to   = [prometheus.relabel.filter.receiver]
}}

// ─── kube-state-metrics ──────────────────────────────
prometheus.scrape "kube_state_metrics" {{
  targets = [{{
    __address__ = "kube-state-metrics.monitoring.svc:8080",
  }}]
  scrape_interval = "30s"
  forward_to = [prometheus.relabel.filter.receiver]
}}

// ─── Metric filter: keep only metrics needed for RCA / dashboards ─────
prometheus.relabel "filter" {{
  rule {{
    source_labels = ["__name__"]
    regex = "up|container_cpu_usage_seconds_total|container_memory_working_set_bytes|container_network_receive_bytes_total|container_network_transmit_bytes_total|kube_pod_container_status_restarts_total|kube_pod_status_phase|kube_pod_info|kube_pod_container_status_ready|kube_deployment_status_replicas_available|kube_deployment_spec_replicas|kubelet_running_pods|node_cpu_seconds_total|node_memory_MemAvailable_bytes|node_memory_MemTotal_bytes|http_requests_total|http_request_duration_seconds_bucket|http_request_duration_seconds_count|http_request_duration_seconds_sum|http_server_requests_seconds_bucket|http_server_requests_seconds_count|http_server_requests_seconds_sum|process_cpu_seconds_total|process_resident_memory_bytes|rca_demo_.*"
    action = "keep"
  }}
  forward_to = [prometheus.remote_write.mimir.receiver]
}}

// ─── Remote write to Mimir ─────────────────────────
prometheus.remote_write "mimir" {{
  endpoint {{
    url = "{mimir_push_url}"{mimir_auth}
  }}
}}

// ─── Logs: collect from all pods ────────────────────
loki.source.kubernetes "pods" {{
  targets    = discovery.kubernetes.pods.targets
  forward_to = [loki.write.default.receiver]
}}

loki.write "default" {{
  endpoint {{
    url = "{loki_push_url}"{loki_auth}
  }}
}}

// ─── Traces: receive OTLP ───────────────────────────
otelcol.receiver.otlp "default" {{
  grpc {{
    endpoint = "0.0.0.0:4317"
  }}
  http {{
    endpoint = "0.0.0.0:4318"
  }}
  output {{
    traces = [{traces_output}]
  }}
}}
{tempo_block}
"#
    )
}

/// Fallback config when no telemetry is configured — collect only, no remote write.
fn generate_default_config() -> String {
    r#"// No telemetry backend configured — collecting but not shipping.
// Configure a telemetry provider in the Ops UI to enable shipping.

discovery.kubernetes "pods" {
  role = "pod"
}

discovery.kubernetes "nodes" {
  role = "node"
}
"#
    .to_string()
}

/// Patch the Alloy ConfigMap in k8s with the generated config.
async fn apply_configmap(config: &str) -> AppResult<()> {
    let client = kube::Client::try_default()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to create in-cluster k8s client: {e}")))?;

    let api: kube::Api<k8s_openapi::api::core::v1::ConfigMap> = kube::Api::namespaced(client, NAMESPACE);

    let cm = k8s_openapi::api::core::v1::ConfigMap {
        metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta {
            name: Some(CONFIGMAP_NAME.to_string()),
            namespace: Some(NAMESPACE.to_string()),
            ..Default::default()
        },
        data: Some(std::collections::BTreeMap::from([(
            CONFIGMAP_KEY.to_string(),
            config.to_string(),
        )])),
        ..Default::default()
    };

    api.patch(
        CONFIGMAP_NAME,
        &kube::api::PatchParams::apply("opsk-backend").force(),
        &kube::api::Patch::Apply(cm),
    )
    .await
    .map_err(|e| AppError::Internal(format!("Failed to patch Alloy ConfigMap: {e}")))?;

    tracing::info!("Patched ConfigMap {}/{}", NAMESPACE, CONFIGMAP_NAME);
    Ok(())
}

/// Restart Alloy DaemonSet by annotating the pod template.
async fn restart_daemonset() -> AppResult<()> {
    let client = kube::Client::try_default()
        .await
        .map_err(|e| AppError::Internal(format!("Failed to create in-cluster k8s client: {e}")))?;

    let api: kube::Api<k8s_openapi::api::apps::v1::DaemonSet> = kube::Api::namespaced(client, NAMESPACE);

    let now = chrono::Utc::now().to_rfc3339();
    let patch = serde_json::json!({
        "spec": {
            "template": {
                "metadata": {
                    "annotations": {
                        "opsk.io/restartedAt": now
                    }
                }
            }
        }
    });

    api.patch(
        DAEMONSET_NAME,
        &kube::api::PatchParams::default(),
        &kube::api::Patch::Merge(patch),
    )
    .await
    .map_err(|e| AppError::Internal(format!("Failed to restart Alloy DaemonSet: {e}")))?;

    tracing::info!("Triggered Alloy DaemonSet restart");
    Ok(())
}
