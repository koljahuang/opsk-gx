//! MCP JSON-RPC server for RCA investigation tools.
//!
//! Provides high-level investigation tools that handle query construction internally.
//! The AI agent specifies *what* to investigate (service, namespace, keywords),
//! not *how* (no raw LogQL/PromQL/TraceQL required).

use axum::http::HeaderMap;
use axum::{Json, extract::Query, extract::State};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::services::grafana_query::GrafanaClients;

// ─── JSON-RPC types ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: Option<String>,
    pub id: Option<serde_json::Value>,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: Some(result),
            error: None,
        }
    }
    fn error(id: Option<serde_json::Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0".to_string(),
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

// ─── Tool definitions ───────────────────────────────────────────────────────

fn tools_list() -> serde_json::Value {
    serde_json::json!({
        "tools": [
            {
                "name": "discover_data_sources",
                "description": "Discover what data exists in the monitoring system. Returns available Loki labels, their values, and available Prometheus metric names. Use this FIRST to understand what services, namespaces, and labels are available before querying.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "label": {
                            "type": "string",
                            "description": "Optional: get values for a specific Loki label (e.g. 'namespace', 'service_name', 'job'). If empty, returns all available label names."
                        },
                        "metric_filter": {
                            "type": "string",
                            "description": "Optional: filter Prometheus metric names containing this string (e.g. 'http', 'error', 'cpu')"
                        }
                    }
                }
            },
            {
                "name": "check_service_health",
                "description": "Get a comprehensive health overview of a service: HTTP error rate, request rate, p99 latency, container restarts, CPU/memory usage, pod readiness. Use this to quickly assess if a service has problems.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "service": {
                            "type": "string",
                            "description": "Service name (supports fuzzy matching, e.g. 'rca-demo' matches containers/services containing that name)"
                        },
                        "namespace": {
                            "type": "string",
                            "description": "Kubernetes namespace (optional, helps narrow the search)"
                        },
                        "minutes": {
                            "type": "integer",
                            "description": "Look-back window in minutes (default: 30)",
                            "default": 30
                        }
                    },
                    "required": ["service"]
                }
            },
            {
                "name": "search_logs",
                "description": "Search service logs with automatic LogQL construction. Supports filtering by service, namespace, keywords, and log level. Returns matching log lines.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "service": {
                            "type": "string",
                            "description": "Service name to search logs for (fuzzy match)"
                        },
                        "namespace": {
                            "type": "string",
                            "description": "Kubernetes namespace (optional)"
                        },
                        "keywords": {
                            "type": "string",
                            "description": "Comma-separated keywords to filter logs (e.g. 'error,timeout,connection refused')"
                        },
                        "level": {
                            "type": "string",
                            "description": "Log level filter (e.g. 'error', 'warn', 'fatal')"
                        },
                        "minutes": {
                            "type": "integer",
                            "description": "Look-back window in minutes (default: 60)",
                            "default": 60
                        }
                    }
                }
            },
            {
                "name": "query_metrics",
                "description": "Run custom PromQL queries when the standard health check doesn't cover what you need. Use check_service_health first; only use this for specific follow-up queries.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "queries": {
                            "type": "array",
                            "description": "List of {name, promql} objects to query",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "name": { "type": "string" },
                                    "promql": { "type": "string" }
                                },
                                "required": ["name", "promql"]
                            }
                        },
                        "minutes": {
                            "type": "integer",
                            "description": "Look-back window in minutes (default: 30)",
                            "default": 30
                        }
                    },
                    "required": ["queries"]
                }
            },
            {
                "name": "search_traces",
                "description": "Search for distributed traces. Finds slow or errored requests across services.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "service": {
                            "type": "string",
                            "description": "Service name to search traces for"
                        },
                        "status": {
                            "type": "string",
                            "description": "Trace status filter: 'error', 'ok', or empty for all",
                            "default": "error"
                        },
                        "min_duration_ms": {
                            "type": "integer",
                            "description": "Minimum span duration in milliseconds (useful for finding slow requests)"
                        },
                        "minutes": {
                            "type": "integer",
                            "description": "Look-back window in minutes (default: 30)",
                            "default": 30
                        }
                    }
                }
            },
            {
                "name": "fetch_source_code",
                "description": "Fetch source code from the GitHub repository. Use when you've identified a file path and line number from logs or traces and need to examine the actual code.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "file_path": {
                            "type": "string",
                            "description": "Path to the file in the repository"
                        },
                        "start_line": {
                            "type": "integer",
                            "description": "Starting line number (0 for entire file)",
                            "default": 0
                        },
                        "end_line": {
                            "type": "integer",
                            "description": "Ending line number (0 for entire file)",
                            "default": 0
                        }
                    },
                    "required": ["file_path"]
                }
            }
        ]
    })
}

// ─── POST /api/mcp/rca — JSON-RPC handler ──────────────────────────────────

#[derive(Debug, Deserialize)]
pub struct McpRcaQuery {
    pub issue_time: Option<String>,
}

pub async fn handle(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<McpRcaQuery>,
    Json(req): Json<JsonRpcRequest>,
) -> AppResult<Json<JsonRpcResponse>> {
    // Try header first, then query param
    let raw_header = headers
        .get("X-Issue-Time")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    let raw_value = raw_header.or(query.issue_time);
    let anchor: Option<DateTime<Utc>> = raw_value.as_deref().and_then(|s| s.parse::<DateTime<Utc>>().ok());

    if req.method == "tools/call" {
        let tool = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("?");
        tracing::info!("MCP RCA tool={} issue_time={:?} anchor={:?}", tool, raw_value, anchor);
    }

    match req.method.as_str() {
        "tools/list" => Ok(Json(JsonRpcResponse::success(req.id, tools_list()))),
        "tools/call" => {
            let tool_name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let arguments = req
                .params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));

            let result = call_tool(&state, &auth_user, tool_name, &arguments, anchor).await;

            match result {
                Ok(content) => Ok(Json(JsonRpcResponse::success(
                    req.id,
                    serde_json::json!({
                        "content": [{ "type": "text", "text": content }]
                    }),
                ))),
                Err(e) => Ok(Json(JsonRpcResponse::error(req.id, -32000, e.to_string()))),
            }
        }
        "initialize" => Ok(Json(JsonRpcResponse::success(
            req.id,
            serde_json::json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "opsk-rca", "version": "2.0.0" }
            }),
        ))),
        _ => Ok(Json(JsonRpcResponse::error(
            req.id,
            -32601,
            format!("Method not found: {}", req.method),
        ))),
    }
}

// ─── Tool dispatch ──────────────────────────────────────────────────────────

async fn call_tool(
    state: &AppState,
    _auth_user: &AuthUser,
    tool_name: &str,
    args: &serde_json::Value,
    anchor: Option<DateTime<Utc>>,
) -> Result<String, AppError> {
    match tool_name {
        "discover_data_sources" => {
            let gc = load_grafana_clients(&state.pool).await?;
            let label = args.get("label").and_then(|v| v.as_str()).unwrap_or("");
            let metric_filter = args.get("metric_filter").and_then(|v| v.as_str()).unwrap_or("");

            let mut results = Vec::new();

            if label.is_empty() {
                results.push(crate::services::grafana_query::discover_loki_labels(&gc).await);
            } else {
                results.push(crate::services::grafana_query::discover_loki_label_values(&gc, label).await);
            }

            if !metric_filter.is_empty() || label.is_empty() {
                results.push(crate::services::grafana_query::discover_mimir_metrics(&gc, metric_filter).await);
            }

            Ok(results.join("\n\n"))
        }

        "check_service_health" => {
            let gc = load_grafana_clients(&state.pool).await?;
            let service = args
                .get("service")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::BadRequest("service is required".to_string()))?;
            let namespace = args.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
            let minutes = args.get("minutes").and_then(|v| v.as_i64()).unwrap_or(30);

            Ok(crate::services::grafana_query::check_service_health(&gc, service, namespace, minutes, anchor).await)
        }

        "search_logs" => {
            let gc = load_grafana_clients(&state.pool).await?;
            let service = args.get("service").and_then(|v| v.as_str()).unwrap_or("");
            let namespace = args.get("namespace").and_then(|v| v.as_str()).unwrap_or("");
            let keywords = args.get("keywords").and_then(|v| v.as_str()).unwrap_or("");
            let level = args.get("level").and_then(|v| v.as_str()).unwrap_or("");
            let minutes = args.get("minutes").and_then(|v| v.as_i64()).unwrap_or(60);

            Ok(
                crate::services::grafana_query::search_logs(&gc, service, namespace, keywords, level, minutes, anchor)
                    .await,
            )
        }

        "query_metrics" => {
            let gc = load_grafana_clients(&state.pool).await?;
            let queries_arr = args
                .get("queries")
                .and_then(|v| v.as_array())
                .ok_or_else(|| AppError::BadRequest("queries array is required".to_string()))?;
            let minutes = args.get("minutes").and_then(|v| v.as_i64()).unwrap_or(30);

            let queries: Vec<(String, String)> = queries_arr
                .iter()
                .filter_map(|q| {
                    let name = q.get("name")?.as_str()?.to_string();
                    let promql = q.get("promql")?.as_str()?.to_string();
                    Some((name, promql))
                })
                .collect();

            let refs: Vec<(&str, &str)> = queries.iter().map(|(n, p)| (n.as_str(), p.as_str())).collect();
            let (text, _json) = crate::services::grafana_query::query_mimir_batch(&gc, &refs, minutes, anchor).await;
            Ok(text)
        }

        "search_traces" => {
            let gc = load_grafana_clients(&state.pool).await?;
            let service = args.get("service").and_then(|v| v.as_str()).unwrap_or("");
            let status = args.get("status").and_then(|v| v.as_str()).unwrap_or("error");
            let min_duration = args.get("min_duration_ms").and_then(|v| v.as_u64());
            let minutes = args.get("minutes").and_then(|v| v.as_i64()).unwrap_or(30);

            let mut conditions = Vec::new();
            if !service.is_empty() {
                conditions.push(format!(r#"resource.service.name=~".*{}.*""#, service));
            }
            if !status.is_empty() {
                conditions.push(format!("status={}", status));
            }
            if let Some(ms) = min_duration {
                conditions.push(format!("duration>{}ms", ms));
            }

            let traceql = if conditions.is_empty() {
                "{}".to_string()
            } else {
                format!("{{{}}}", conditions.join(" && "))
            };

            let (text, _json) = crate::services::grafana_query::query_tempo(&gc, &traceql, minutes, 10, anchor).await;
            Ok(format!("TraceQL: {}\n\n{}", traceql, text))
        }

        "fetch_source_code" => {
            let file_path = args
                .get("file_path")
                .and_then(|v| v.as_str())
                .ok_or_else(|| AppError::BadRequest("file_path is required".to_string()))?;
            let start_line = args.get("start_line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
            let end_line = args.get("end_line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;

            let (repo, token) = load_github_config(&state.pool, &state.config).await?;

            crate::services::github::fetch_source_range(&repo, &token, file_path, start_line, end_line)
                .await
                .map_err(AppError::Internal)
        }

        _ => Err(AppError::BadRequest(format!("Unknown tool: {tool_name}"))),
    }
}

async fn load_github_config(
    pool: &sqlx::PgPool,
    config: &crate::config::AppConfig,
) -> Result<(String, String), AppError> {
    // Try DB first: pipeline_repos with enabled=true
    let row: Option<(String, Option<String>)> = sqlx::query_as(
        "SELECT repository, token_secret_arn FROM pipeline_repos WHERE enabled = true ORDER BY created_at LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| AppError::Internal(e.to_string()))?;

    if let Some((repo_url, secret_arn)) = row {
        let owner_repo = repo_url
            .trim_end_matches('/')
            .trim_end_matches(".git")
            .rsplit("github.com/")
            .next()
            .unwrap_or(&repo_url)
            .to_string();

        let token = if let Some(val) = secret_arn.filter(|a| !a.is_empty()) {
            if val.starts_with("arn:") {
                crate::services::pipeline::fetch_secret_value(&val).await.map_err(|e| {
                    AppError::Internal(format!("Failed to fetch GitHub token from Secrets Manager: {e}"))
                })?
            } else {
                val
            }
        } else {
            return Err(AppError::BadRequest(
                "GitHub repo found but no PAT token configured".to_string(),
            ));
        };

        return Ok((owner_repo, token));
    }

    // Fallback to env vars
    let token = config
        .github_token
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("No GitHub repo configured. Add one in Settings → Repos.".to_string()))?;
    let repo = config
        .github_repo
        .as_deref()
        .ok_or_else(|| AppError::BadRequest("GITHUB_REPO not configured".to_string()))?;
    Ok((repo.to_string(), token.to_string()))
}

async fn load_grafana_clients(pool: &sqlx::PgPool) -> Result<GrafanaClients, AppError> {
    GrafanaClients::load(pool).await.ok_or_else(|| {
        AppError::BadRequest(
            "Telemetry (Grafana Cloud) not configured. Please set up telemetry in Settings.".to_string(),
        )
    })
}
