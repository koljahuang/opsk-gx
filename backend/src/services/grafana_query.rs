//! Query Grafana Cloud APIs (Loki, Mimir, Tempo) using stored telemetry_config credentials.
//!
//! Each public function accepts a pre-loaded `GrafanaClients` so the caller can
//! load config + build the HTTP client once and reuse across all queries.

use chrono::{DateTime, Utc};
use reqwest::Client;
use sqlx::PgPool;

use crate::models::telemetry::TelemetryConfig;

/// Resolve a (start, end) time range.
/// If `anchor` is provided, use it as the end time; otherwise use `now`.
pub fn resolve_time_range(minutes: i64, anchor: Option<DateTime<Utc>>) -> (DateTime<Utc>, DateTime<Utc>) {
    let end = anchor.unwrap_or_else(Utc::now);
    let start = end - chrono::Duration::minutes(minutes);
    (start, end)
}

/// Pre-loaded Grafana Cloud credentials and a shared HTTP client.
/// Created once per RCA run via `GrafanaClients::load()`.
pub struct GrafanaClients {
    pub client: Client,
    pub loki_url: String,
    pub loki_user: String,
    pub mimir_url: String,
    pub mimir_user: String,
    pub tempo_url: String,
    pub tempo_user: String,
    pub api_token: String,
}

impl GrafanaClients {
    /// Load the first enabled telemetry config from DB and build clients.
    /// Returns None if no config is found or credentials are empty.
    pub async fn load(pool: &PgPool) -> Option<Self> {
        let tc = sqlx::query_as::<_, TelemetryConfig>(
            "SELECT * FROM telemetry_config WHERE enabled = true ORDER BY created_at ASC LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()?;

        let c = &tc.config;
        let api_token = cfg_str(c, "api_token").to_string();
        if api_token.is_empty() {
            return None;
        }

        Some(Self {
            client: Client::new(),
            loki_url: cfg_str(c, "loki_endpoint_url").to_string(),
            loki_user: cfg_str(c, "loki_user_id").to_string(),
            mimir_url: cfg_str(c, "mimir_endpoint_url").to_string(),
            mimir_user: cfg_str(c, "mimir_user_id").to_string(),
            tempo_url: cfg_str(c, "tempo_endpoint_url").to_string(),
            tempo_user: cfg_str(c, "tempo_user_id").to_string(),
            api_token,
        })
    }
}

/// Extract a string field from config JSONB.
fn cfg_str<'a>(config: &'a serde_json::Value, key: &str) -> &'a str {
    config.get(key).and_then(|v| v.as_str()).unwrap_or("")
}

/// Build the query base URL, adding service-specific prefix for Grafana Cloud.
/// Grafana Cloud endpoints (e.g. `https://prometheus-prod-xxx.grafana.net`) need
/// `/prometheus`, `/tempo`, etc. before the standard API paths.
/// Also strips push-only suffixes like `/api/prom/push` that are meant for write paths.
fn query_base(endpoint: &str, service_prefix: &str) -> String {
    let mut base = endpoint.trim_end_matches('/').to_string();
    // Strip known push-only paths (stored for Alloy write, not for query)
    for suffix in &["/api/prom/push", "/api/prom", "/push", "/api/v1/push"] {
        if base.ends_with(suffix) {
            base.truncate(base.len() - suffix.len());
            break;
        }
    }
    let base = base.trim_end_matches('/');
    if base.contains(".grafana.net") && !base.ends_with(service_prefix) {
        format!("{}{}", base, service_prefix)
    } else {
        base.to_string()
    }
}

// ─── Loki ────────────────────────────────────────────────────

/// Query Loki with a custom LogQL expression.
/// Returns (text_summary, raw_json) for both human and structured consumption.
pub async fn query_loki(
    gc: &GrafanaClients,
    logql: &str,
    minutes: i64,
    limit: u32,
    anchor: Option<DateTime<Utc>>,
) -> (String, serde_json::Value) {
    if gc.loki_url.is_empty() {
        return ("(Loki not configured)".to_string(), serde_json::json!(null));
    }

    let (start, end) = resolve_time_range(minutes, anchor);
    let url = format!("{}/api/v1/query_range", query_base(&gc.loki_url, "/loki"));

    let resp = gc
        .client
        .get(&url)
        .basic_auth(&gc.loki_user, Some(&gc.api_token))
        .query(&[
            ("query", logql),
            ("start", &start.timestamp().to_string()),
            ("end", &end.timestamp().to_string()),
            ("limit", &limit.to_string()),
        ])
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let text = format_loki_results(&body);
            (text, body)
        }
        Ok(r) => {
            let msg = format!("(Loki query failed: HTTP {})", r.status());
            (msg.clone(), serde_json::json!({"error": msg}))
        }
        Err(e) => {
            let msg = format!("(Loki query error: {})", e);
            (msg.clone(), serde_json::json!({"error": msg}))
        }
    }
}

/// Query Loki for recent error logs of a given service (convenience wrapper).
pub async fn query_loki_errors(gc: &GrafanaClients, service: &str, minutes: i64) -> String {
    let logql = format!(r#"{{service="{service}"}} |= "error" | json | level="error""#);
    query_loki(gc, &logql, minutes, 50, None).await.0
}

/// Format Loki query_range response into readable text.
fn format_loki_results(body: &serde_json::Value) -> String {
    let mut lines = Vec::new();
    if let Some(results) = body
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array())
    {
        for stream in results {
            if let Some(values) = stream.get("values").and_then(|v| v.as_array()) {
                for entry in values.iter().rev().take(30) {
                    if let Some(log_line) = entry.get(1).and_then(|v| v.as_str()) {
                        lines.push(log_line.to_string());
                    }
                }
            }
        }
    }

    if lines.is_empty() {
        "(No error logs found in the last query window)".to_string()
    } else {
        format!("Found {} error log entries:\n\n{}", lines.len(), lines.join("\n"))
    }
}

// ─── Mimir (shared helper) ──────────────────────────────────

/// Run a batch of PromQL range queries against Mimir and return formatted results.
/// Shared by application metrics, container resources, and node resources.
async fn run_mimir_queries(
    gc: &GrafanaClients,
    queries: &[(&str, String)],
    minutes: i64,
    step: &str,
    skip_empty: bool,
    anchor: Option<DateTime<Utc>>,
) -> String {
    if gc.mimir_url.is_empty() {
        return "(Mimir not configured)".to_string();
    }

    let (start, end) = resolve_time_range(minutes, anchor);
    let url = format!("{}/api/v1/query_range", query_base(&gc.mimir_url, "/prometheus"));

    let mut results = Vec::new();

    for (name, promql) in queries {
        let resp = gc
            .client
            .get(&url)
            .basic_auth(&gc.mimir_user, Some(&gc.api_token))
            .query(&[
                ("query", promql.as_str()),
                ("start", &start.timestamp().to_string()),
                ("end", &end.timestamp().to_string()),
                ("step", step),
            ])
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let body: serde_json::Value = r.json().await.unwrap_or_default();
                let summary = format_mimir_result(name, &body);
                if !skip_empty || summary.lines().count() > 1 {
                    results.push(summary);
                }
            }
            Ok(r) if !skip_empty => {
                results.push(format!("{}: HTTP {}", name, r.status()));
            }
            Err(e) if !skip_empty => {
                results.push(format!("{}: error {}", name, e));
            }
            _ => { /* skip non-critical failures when skip_empty=true */ }
        }
    }

    if results.is_empty() {
        "(No metrics data available)".to_string()
    } else {
        results.join("\n\n")
    }
}

/// Run a single PromQL query against Mimir, return (text, raw_json).
pub async fn query_mimir_single(
    gc: &GrafanaClients,
    name: &str,
    promql: &str,
    minutes: i64,
    anchor: Option<DateTime<Utc>>,
) -> (String, serde_json::Value) {
    if gc.mimir_url.is_empty() {
        return ("(Mimir not configured)".to_string(), serde_json::json!(null));
    }

    let (start, end) = resolve_time_range(minutes, anchor);
    let url = format!("{}/api/v1/query_range", query_base(&gc.mimir_url, "/prometheus"));

    let resp = gc
        .client
        .get(&url)
        .basic_auth(&gc.mimir_user, Some(&gc.api_token))
        .query(&[
            ("query", promql),
            ("start", &start.timestamp().to_string()),
            ("end", &end.timestamp().to_string()),
            ("step", "30"),
        ])
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let text = format_mimir_result(name, &body);
            (text, body)
        }
        Ok(r) => {
            let msg = format!("(Mimir query failed: HTTP {})", r.status());
            (msg.clone(), serde_json::json!({"error": msg}))
        }
        Err(e) => {
            let msg = format!("(Mimir query error: {})", e);
            (msg.clone(), serde_json::json!({"error": msg}))
        }
    }
}

/// Run a batch of named PromQL queries, return (text, raw_json_array).
pub async fn query_mimir_batch(
    gc: &GrafanaClients,
    queries: &[(&str, &str)],
    minutes: i64,
    anchor: Option<DateTime<Utc>>,
) -> (String, serde_json::Value) {
    let mut texts = Vec::new();
    let mut results = serde_json::Map::new();

    for (name, promql) in queries {
        let (text, json) = query_mimir_single(gc, name, promql, minutes, anchor).await;
        texts.push(text);
        results.insert(name.to_string(), json);
    }

    let combined = if texts.is_empty() {
        "(No metrics data available)".to_string()
    } else {
        texts.join("\n\n")
    };
    (combined, serde_json::Value::Object(results))
}

/// Query Mimir for error rate and latency metrics of a given service.
pub async fn query_mimir_metrics(gc: &GrafanaClients, service: &str, minutes: i64) -> String {
    let queries = vec![
        (
            "error_rate",
            format!(r#"rate(orders_errors_total{{service="{service}"}}[2m])"#),
        ),
        (
            "total_orders",
            format!(r#"rate(orders_total{{service="{service}"}}[2m])"#),
        ),
        (
            "p99_latency",
            format!(
                r#"histogram_quantile(0.99, rate(order_processing_duration_ms_bucket{{service="{service}"}}[2m]))"#
            ),
        ),
    ];
    run_mimir_queries(gc, &queries, minutes, "30", false, None).await
}

/// Query Mimir for container-level resource metrics.
pub async fn query_container_resources(gc: &GrafanaClients, service: &str, minutes: i64) -> String {
    let queries = vec![
        (
            "container_cpu_usage",
            format!(r#"rate(container_cpu_usage_seconds_total{{container="{service}"}}[2m])"#),
        ),
        (
            "container_memory_bytes",
            format!(r#"container_memory_working_set_bytes{{container="{service}"}}"#),
        ),
        (
            "container_memory_limit",
            format!(r#"kube_pod_container_resource_limits{{container="{service}", resource="memory"}}"#),
        ),
        (
            "container_restarts",
            format!(r#"kube_pod_container_status_restarts_total{{container="{service}"}}"#),
        ),
        (
            "container_oom_killed",
            format!(r#"kube_pod_container_status_last_terminated_reason{{container="{service}", reason="OOMKilled"}}"#),
        ),
    ];
    run_mimir_queries(gc, &queries, minutes, "60", true, None).await
}

/// Query Mimir for node-level resource metrics (CPU, memory, disk I/O, network).
pub async fn query_node_resources(gc: &GrafanaClients, minutes: i64) -> String {
    let queries = vec![
        ("node_cpu_usage", "1 - avg(rate(node_cpu_seconds_total{mode=\"idle\"}[2m])) by (instance)".to_string()),
        ("node_memory_usage", "1 - (node_memory_MemAvailable_bytes / node_memory_MemTotal_bytes)".to_string()),
        ("node_disk_io_util", "rate(node_disk_io_time_seconds_total[2m])".to_string()),
        ("node_network_receive_bytes", r#"rate(node_network_receive_bytes_total{device!~"lo|veth.*|docker.*|flannel.*|cni.*"}[2m])"#.to_string()),
        ("node_network_transmit_bytes", r#"rate(node_network_transmit_bytes_total{device!~"lo|veth.*|docker.*|flannel.*|cni.*"}[2m])"#.to_string()),
        ("node_disk_space_usage", r#"1 - (node_filesystem_avail_bytes{mountpoint="/",fstype!~"tmpfs|fuse.*"} / node_filesystem_size_bytes{mountpoint="/",fstype!~"tmpfs|fuse.*"})"#.to_string()),
    ];
    run_mimir_queries(gc, &queries, minutes, "120", true, None).await
}

/// Format a single Mimir query_range result.
fn format_mimir_result(name: &str, body: &serde_json::Value) -> String {
    let mut lines = vec![format!("### {}", name)];

    if let Some(results) = body
        .get("data")
        .and_then(|d| d.get("result"))
        .and_then(|r| r.as_array())
    {
        for series in results {
            let labels = series
                .get("metric")
                .map(|m| serde_json::to_string(m).unwrap_or_default())
                .unwrap_or_default();

            if let Some(values) = series.get("values").and_then(|v| v.as_array()) {
                let last_values: Vec<String> = values
                    .iter()
                    .rev()
                    .take(5)
                    .rev()
                    .filter_map(|pair| {
                        let ts = pair.get(0)?.as_f64()?;
                        let val = pair.get(1)?.as_str()?;
                        let time = chrono::DateTime::from_timestamp(ts as i64, 0)
                            .map(|dt| dt.format("%H:%M:%S").to_string())
                            .unwrap_or_else(|| ts.to_string());
                        Some(format!("  {} → {}", time, val))
                    })
                    .collect();

                if !last_values.is_empty() {
                    if !labels.is_empty() && labels != "{}" {
                        lines.push(format!("Labels: {}", labels));
                    }
                    lines.push("Recent values:".to_string());
                    lines.extend(last_values);
                }
            }
        }
    }

    lines.join("\n")
}

// ─── Tempo ───────────────────────────────────────────────────

/// Query Tempo with a custom TraceQL expression. Returns (text, raw_json).
pub async fn query_tempo(
    gc: &GrafanaClients,
    traceql: &str,
    minutes: i64,
    limit: u32,
    anchor: Option<DateTime<Utc>>,
) -> (String, serde_json::Value) {
    if gc.tempo_url.is_empty() {
        return ("(Tempo not configured)".to_string(), serde_json::json!(null));
    }

    let (start, end) = resolve_time_range(minutes, anchor);
    let url = format!("{}/api/search", query_base(&gc.tempo_url, "/tempo"));

    let resp = gc
        .client
        .get(&url)
        .basic_auth(&gc.tempo_user, Some(&gc.api_token))
        .query(&[
            ("q", traceql),
            ("start", &start.timestamp().to_string()),
            ("end", &end.timestamp().to_string()),
            ("limit", &limit.to_string()),
        ])
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let text = format_tempo_results(&body);
            (text, body)
        }
        Ok(r) => {
            let msg = format!("(Tempo query failed: HTTP {})", r.status());
            (msg.clone(), serde_json::json!({"error": msg}))
        }
        Err(e) => {
            let msg = format!("(Tempo query error: {})", e);
            (msg.clone(), serde_json::json!({"error": msg}))
        }
    }
}

/// Query Tempo for recent error traces of a given service.
pub async fn query_tempo_errors(gc: &GrafanaClients, service: &str, minutes: i64) -> String {
    if gc.tempo_url.is_empty() {
        return "(Tempo not configured)".to_string();
    }

    let (start, end) = resolve_time_range(minutes, None);

    let url = format!("{}/api/search", query_base(&gc.tempo_url, "/tempo"));
    let traceql = format!(r#"{{status=error && resource.service.name="{service}"}}"#);

    let resp = gc
        .client
        .get(&url)
        .basic_auth(&gc.tempo_user, Some(&gc.api_token))
        .query(&[
            ("q", traceql.as_str()),
            ("start", &start.timestamp().to_string()),
            ("end", &end.timestamp().to_string()),
            ("limit", "10"),
        ])
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            format_tempo_results(&body)
        }
        Ok(r) => format!("(Tempo query failed: HTTP {})", r.status()),
        Err(e) => format!("(Tempo query error: {})", e),
    }
}

/// Format Tempo search response into readable text.
fn format_tempo_results(body: &serde_json::Value) -> String {
    let traces = body.get("traces").and_then(|t| t.as_array());

    let traces = match traces {
        Some(t) if !t.is_empty() => t,
        _ => return "(No error traces found in the last query window)".to_string(),
    };

    let mut lines = vec![format!("Found {} error traces:", traces.len())];

    for trace in traces.iter().take(10) {
        let trace_id = trace.get("traceID").and_then(|v| v.as_str()).unwrap_or("unknown");
        let root_service = trace
            .get("rootServiceName")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let root_name = trace.get("rootTraceName").and_then(|v| v.as_str()).unwrap_or("unknown");
        let duration_ms = trace.get("durationMs").and_then(|v| v.as_u64()).unwrap_or(0);
        let start_time = trace.get("startTimeUnixNano").and_then(|v| v.as_str()).unwrap_or("");

        lines.push(format!(
            "\n- TraceID: {}\n  Service: {} | Span: {} | Duration: {}ms\n  StartTime: {}",
            trace_id, root_service, root_name, duration_ms, start_time
        ));

        if let Some(span_sets) = trace.get("spanSets").and_then(|s| s.as_array()) {
            for span_set in span_sets {
                if let Some(spans) = span_set.get("spans").and_then(|s| s.as_array()) {
                    for span in spans {
                        if let Some(attrs) = span.get("attributes").and_then(|a| a.as_array()) {
                            for attr in attrs {
                                let key = attr.get("key").and_then(|k| k.as_str()).unwrap_or("");
                                let val = attr
                                    .get("value")
                                    .and_then(|v| v.get("stringValue"))
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("");
                                if !key.is_empty() && !val.is_empty() {
                                    lines.push(format!("  {}: {}", key, val));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    lines.join("\n")
}

// ─── Discovery ──────────────────────────────────────────────

/// Query Loki /labels to discover available label names.
pub async fn discover_loki_labels(gc: &GrafanaClients) -> String {
    if gc.loki_url.is_empty() {
        return "(Loki not configured)".to_string();
    }
    let url = format!("{}/api/v1/labels", query_base(&gc.loki_url, "/loki"));
    match gc
        .client
        .get(&url)
        .basic_auth(&gc.loki_user, Some(&gc.api_token))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let labels = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            format!("Available Loki labels: {}", labels)
        }
        Ok(r) => format!("(Loki labels failed: HTTP {})", r.status()),
        Err(e) => format!("(Loki labels error: {})", e),
    }
}

/// Query Loki /label/{name}/values to discover values for a label.
pub async fn discover_loki_label_values(gc: &GrafanaClients, label: &str) -> String {
    if gc.loki_url.is_empty() {
        return "(Loki not configured)".to_string();
    }
    let url = format!("{}/api/v1/label/{}/values", query_base(&gc.loki_url, "/loki"), label);
    match gc
        .client
        .get(&url)
        .basic_auth(&gc.loki_user, Some(&gc.api_token))
        .send()
        .await
    {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let values = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            format!("Values for label '{}': {}", label, values)
        }
        Ok(r) => format!("(Loki label values failed: HTTP {})", r.status()),
        Err(e) => format!("(Loki label values error: {})", e),
    }
}

/// Query Mimir to discover available metric names. Tries label values API first,
/// falls back to a simple test query if that fails.
pub async fn discover_mimir_metrics(gc: &GrafanaClients, filter: &str) -> String {
    if gc.mimir_url.is_empty() {
        return "(Mimir not configured)".to_string();
    }
    let base = query_base(&gc.mimir_url, "/prometheus");

    // Try label values API first
    let url = format!("{}/api/v1/label/__name__/values", base);
    let resp = gc
        .client
        .get(&url)
        .basic_auth(&gc.mimir_user, Some(&gc.api_token))
        .send()
        .await;

    match resp {
        Ok(r) if r.status().is_success() => {
            let body: serde_json::Value = r.json().await.unwrap_or_default();
            let metrics: Vec<&str> = body
                .get("data")
                .and_then(|d| d.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .filter(|name| filter.is_empty() || name.to_lowercase().contains(&filter.to_lowercase()))
                        .collect()
                })
                .unwrap_or_default();
            if metrics.is_empty() {
                format!("Mimir is reachable but no metrics found matching '{}'", filter)
            } else {
                format!(
                    "Found {} metrics{}:\n{}",
                    metrics.len(),
                    if filter.is_empty() {
                        String::new()
                    } else {
                        format!(" matching '{}'", filter)
                    },
                    metrics.iter().take(50).cloned().collect::<Vec<_>>().join("\n")
                )
            }
        }
        Ok(r) => {
            let status = r.status();
            // Fallback: try a simple instant query to test connectivity
            let test_url = format!("{}/api/v1/query", base);
            let test_resp = gc
                .client
                .get(&test_url)
                .basic_auth(&gc.mimir_user, Some(&gc.api_token))
                .query(&[("query", "up"), ("time", &Utc::now().timestamp().to_string())])
                .send()
                .await;
            match test_resp {
                Ok(tr) if tr.status().is_success() => {
                    let body: serde_json::Value = tr.json().await.unwrap_or_default();
                    let count = body
                        .get("data")
                        .and_then(|d| d.get("result"))
                        .and_then(|r| r.as_array())
                        .map(|a| a.len())
                        .unwrap_or(0);
                    format!(
                        "Mimir labels API returned HTTP {} but query API works. Found {} 'up' series. Use check_service_health or query_metrics for specific queries.",
                        status, count
                    )
                }
                Ok(tr) => format!(
                    "(Mimir not reachable: labels={}, query={}, url={})",
                    status,
                    tr.status(),
                    base
                ),
                Err(e) => format!("(Mimir not reachable: labels={}, error={}, url={})", status, e, base),
            }
        }
        Err(e) => format!("(Mimir discovery error: {})", e),
    }
}

/// High-level: check service health by querying common metrics patterns.
pub async fn check_service_health(
    gc: &GrafanaClients,
    service: &str,
    namespace: &str,
    minutes: i64,
    anchor: Option<DateTime<Utc>>,
) -> String {
    let ns_filter = if namespace.is_empty() {
        String::new()
    } else {
        format!(r#", namespace="{namespace}""#)
    };
    let svc = service;
    let queries = vec![
        (
            "http_error_rate",
            format!(
                r#"sum(rate(http_requests_total{{service=~".*{svc}.*"{ns_filter}, status=~"5.."}}[5m])) / sum(rate(http_requests_total{{service=~".*{svc}.*"{ns_filter}}}[5m]))"#
            ),
        ),
        (
            "http_request_rate",
            format!(r#"sum(rate(http_requests_total{{service=~".*{svc}.*"{ns_filter}}}[5m]))"#),
        ),
        (
            "p99_latency",
            format!(
                r#"histogram_quantile(0.99, sum(rate(http_request_duration_seconds_bucket{{service=~".*{svc}.*"{ns_filter}}}[5m])) by (le))"#
            ),
        ),
        (
            "container_restarts",
            format!(r#"sum(kube_pod_container_status_restarts_total{{container=~".*{svc}.*"{ns_filter}}})"#),
        ),
        (
            "container_cpu",
            format!(r#"sum(rate(container_cpu_usage_seconds_total{{container=~".*{svc}.*"{ns_filter}}}[5m]))"#),
        ),
        (
            "container_memory_mb",
            format!(r#"sum(container_memory_working_set_bytes{{container=~".*{svc}.*"{ns_filter}}}) / 1024 / 1024"#),
        ),
        (
            "pod_ready",
            format!(r#"sum(kube_pod_status_ready{{pod=~".*{svc}.*"{ns_filter}, condition="true"}})"#),
        ),
    ];

    run_mimir_queries(gc, &queries, minutes, "60", true, anchor).await
}

/// High-level: search logs by service with optional keyword and level filters.
pub async fn search_logs(
    gc: &GrafanaClients,
    service: &str,
    namespace: &str,
    keywords: &str,
    level: &str,
    minutes: i64,
    anchor: Option<DateTime<Utc>>,
) -> String {
    let mut selectors = Vec::new();
    if !service.is_empty() {
        selectors.push(format!(r#"service_name=~".*{}.*""#, service));
    }
    if !namespace.is_empty() {
        selectors.push(format!(r#"namespace="{}""#, namespace));
    }
    if selectors.is_empty() {
        selectors.push("service_name=~\".+\"".to_string());
    }

    let mut pipeline = String::new();
    if !keywords.is_empty() {
        for kw in keywords.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
            pipeline.push_str(&format!(r#" |= "{}""#, kw));
        }
    }
    if !level.is_empty() {
        // Try JSON-structured level filter first; if no results, fall back
        // to a plain-text grep so non-JSON logs (Python tracebacks, etc.)
        // are not silently dropped by the `| json` parser.
        pipeline.push_str(&format!(r#" | json | level=~"(?i){}""#, level));
    }

    let logql = format!("{{{}}}{}", selectors.join(", "), pipeline);
    let (text, _json) = query_loki(gc, &logql, minutes, 100, anchor).await;

    // If the JSON-parsed query returned nothing and a level filter was used,
    // retry with a plain-text case-insensitive grep instead.
    if !level.is_empty() && (text.starts_with("(No") || text.starts_with("(Loki")) {
        let mut fallback_pipeline = String::new();
        if !keywords.is_empty() {
            for kw in keywords.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()) {
                fallback_pipeline.push_str(&format!(r#" |= "{}""#, kw));
            }
        }
        // Use case-insensitive line filter instead of JSON parser
        fallback_pipeline.push_str(&format!(r#" |~ "(?i){}""#, level));

        let fallback_logql = format!("{{{}}}{}", selectors.join(", "), fallback_pipeline);
        let (fallback_text, _) = query_loki(gc, &fallback_logql, minutes, 100, anchor).await;

        if !fallback_text.starts_with("(No") && !fallback_text.starts_with("(Loki") {
            return format!(
                "LogQL: {} (fallback from JSON parse)\n\n{}",
                fallback_logql, fallback_text
            );
        }

        // Both failed — return original result with both queries shown
        return format!("LogQL: {}\n(also tried: {})\n\n{}", logql, fallback_logql, text);
    }

    format!("LogQL: {}\n\n{}", logql, text)
}

// ─── Summarizers ────────────────────────────────────────────

/// Summarize a "Found N ..." result, or truncate to 60 chars.
fn summarize_found_or_truncate(result: &str, fallback: &str) -> String {
    if result.starts_with("Found") {
        result.lines().next().unwrap_or(fallback).to_string()
    } else {
        result.chars().take(60).collect()
    }
}

/// Summarize a metric-count result by counting "###" sections.
fn summarize_section_count(result: &str, label: &str) -> String {
    if result.starts_with('(') {
        return result.chars().take(60).collect();
    }
    let count = result.matches("###").count();
    format!("{} {} collected", count, label)
}

pub fn summarize_loki(result: &str) -> String {
    summarize_found_or_truncate(result, "Error logs found")
}

pub fn summarize_mimir(result: &str) -> String {
    // Extract the last value from the error_rate section
    for line in result.lines() {
        if line.contains('→')
            && let Some(val) = line.split('→').nth(1)
        {
            return format!("Latest error_rate: {}", val.trim());
        }
    }
    result.lines().next().unwrap_or("Metrics fetched").to_string()
}

pub fn summarize_tempo(result: &str) -> String {
    summarize_found_or_truncate(result, "Error traces found")
}

pub fn summarize_container_resources(result: &str) -> String {
    summarize_section_count(result, "container metrics")
}

pub fn summarize_node_resources(result: &str) -> String {
    summarize_section_count(result, "node metrics")
}
