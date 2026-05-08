use axum::{
    Json,
    extract::{Path, State},
    response::sse::{Event, Sse},
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::path::PathBuf;
use std::pin::Pin;
use std::time::Duration;
use tokio_stream::Stream;

use crate::AppState;
use crate::error::{AppError, AppResult};
use crate::middleware::auth::{AuthUser, Claims};
use crate::services::agent::{Agent, AgentEvent, AgentSessionConfig, ImageData as AgentImageData};
use crate::services::claude::{AgentPermission, ClaudeService, StreamChunk};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

#[derive(Debug, Deserialize, Serialize)]
pub struct ChatImage {
    /// Base64-encoded image data
    pub data: String,
    /// MIME type: image/png, image/jpeg, image/gif, image/webp
    pub media_type: String,
    /// Optional filename (used for display in frontend)
    #[serde(default)]
    #[allow(dead_code)]
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ChatRequest {
    pub message: String,
    /// Optional session_id to resume a conversation
    #[serde(default)]
    pub session_id: Option<String>,
    /// Optional system prompt override
    #[serde(default)]
    pub system_prompt: Option<String>,
    /// Optional attached images (base64)
    #[serde(default)]
    pub images: Vec<ChatImage>,
    /// Force new session (skip find_active_session)
    #[serde(default)]
    pub new_session: bool,
    /// Optional provider_id to select a specific model configuration
    #[serde(default)]
    pub provider_id: Option<uuid::Uuid>,
    /// Optional MCP server IDs to include (None = all enabled)
    #[serde(default)]
    pub mcp_server_ids: Option<Vec<uuid::Uuid>>,
    /// Disabled MCP tools in "serverId:toolName" format → mapped to mcp__serverName__toolName
    #[serde(default)]
    pub disabled_mcp_tools: Option<Vec<String>>,
}

type SseEventStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

/// POST /api/chat — SSE streaming endpoint
/// Spawns Claude CLI and streams parsed chunks back as SSE events.
pub async fn stream(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Json(req): Json<ChatRequest>,
) -> Sse<axum::response::sse::KeepAliveStream<SseEventStream>> {
    let claude_bin = state.config.claude_bin.clone();

    // Per-user workspace: {claude_work_dir}/users/{user_id}/
    // Each user gets their own .claude/skills/ with symlinks to authorized skills only.
    let base_work_dir = PathBuf::from(&state.config.claude_work_dir);
    let user_work_dir = base_work_dir.join("users").join(auth_user.user_id.to_string());

    // Read model config from providers table, fallback to env config
    let provider = load_provider_config(&state, auth_user.tenant_id, req.provider_id).await;

    tracing::info!(
        "Provider config: model={}, provider_id={:?}, permission={}, disallowed={:?}, allowed={:?}, env_keys={:?}",
        provider.model,
        req.provider_id,
        provider.permission_mode,
        provider.disallowed_tools,
        provider.allowed_tools,
        provider.env_vars.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()
    );

    // Build per-user .claude/skills/ with symlinks to authorized skills
    setup_user_skill_symlinks(&state, &user_work_dir, auth_user.user_id, auth_user.tenant_id).await;

    // Write CLAUDE.md to user workspace — Claude CLI natively loads this as project instructions.
    // This is far more effective than --system-prompt for controlling agent behavior.
    write_user_claude_md(&state, &auth_user, &user_work_dir).await;

    write_approval_hooks(&user_work_dir).await;

    let service = ClaudeService::new(
        claude_bin,
        user_work_dir.clone(),
        provider.timeout,
        provider.model.clone(),
        provider.max_turns,
        state.pool.clone(),
    );

    // Try to find active session if not provided and not explicitly requesting a new one
    let session_id = if req.new_session {
        tracing::info!("New session requested, skipping find_active_session");
        None
    } else {
        match req.session_id {
            Some(sid) => Some(sid),
            None => {
                service
                    .find_active_session(auth_user.user_id, auth_user.tenant_id)
                    .await
            }
        }
    };

    // Build system prompt with account-level context
    let system_prompt = build_system_prompt(&state, &auth_user, &user_work_dir, req.system_prompt.as_deref()).await;

    // Validate image sizes (max 5MB per image after base64 decode, ~6.7MB base64)
    const MAX_IMAGE_BASE64_LEN: usize = 7 * 1024 * 1024; // ~5MB decoded
    for (i, img) in req.images.iter().enumerate() {
        if img.data.len() > MAX_IMAGE_BASE64_LEN {
            let msg = format!(
                "Image {} exceeds 5MB size limit ({:.1}MB)",
                i + 1,
                img.data.len() as f64 / 1_048_576.0
            );
            tracing::warn!("{}", msg);
            let error_stream = futures::stream::once(async move {
                let chunk = crate::services::claude::StreamChunk::Error { message: msg };
                let data = serde_json::to_string(&chunk).unwrap_or_default();
                Ok::<_, Infallible>(Event::default().data(data))
            });
            return Sse::new(Box::pin(error_stream) as SseEventStream).keep_alive(
                axum::response::sse::KeepAlive::new()
                    .interval(std::time::Duration::from_secs(15))
                    .text("ping"),
            );
        }
    }

    // Convert images for Claude CLI stream-json input
    tracing::info!(
        "Chat request: message={}, images={}, session={:?}",
        req.message.len(),
        req.images.len(),
        session_id,
    );

    let images: Vec<AgentImageData> = req
        .images
        .iter()
        .map(|img| AgentImageData {
            data: img.data.clone(),
            media_type: img.media_type.clone(),
        })
        .collect();

    let user_id = auth_user.user_id;
    let tenant_id = auth_user.tenant_id;
    let pool = state.pool.clone();
    let message_text = req.message.clone();

    // Build env vars: provider config + AWS credentials from cloud accounts
    let mut all_env_vars = provider.env_vars;
    let aws_env_vars = build_aws_env_vars(&state, &auth_user).await;
    all_env_vars.extend(aws_env_vars);

    // Pass DISABLE_LOGIN_COMMAND to Claude CLI subprocess
    if state.config.disable_login_command {
        all_env_vars.push(("DISABLE_LOGIN_COMMAND".to_string(), "1".to_string()));
    }

    // Generate short-lived API token for agent to call Ops APIs.
    // Write an `ops-api` wrapper script so the agent doesn't use $OPS_API_TOKEN
    // directly in curl commands — Claude CLI's security filter hides output when
    // it detects env var expansion ("Contains simple_expansion").
    if let Some(token) = generate_agent_token(&auth_user, &state.config.jwt_secret) {
        let api_base = format!("http://localhost:{}", state.config.backend_port);
        all_env_vars.push(("OPS_API_TOKEN".to_string(), token.clone()));
        all_env_vars.push(("OPS_API_BASE".to_string(), api_base.clone()));

        // Write ops-api helper script to workspace
        write_ops_api_script(&user_work_dir, &token, &api_base).await;
    }

    // Build MCP config from user's enabled MCP servers (writes to file in user_work_dir)
    let api_token_for_mcp = all_env_vars
        .iter()
        .find(|(k, _)| k == "OPS_API_TOKEN")
        .map(|(_, v)| v.clone());
    let (mcp_config, mcp_server_names) = build_mcp_config(
        &state,
        &auth_user,
        &user_work_dir,
        req.mcp_server_ids.as_deref(),
        api_token_for_mcp.as_deref(),
    )
    .await;

    // Auto-allow MCP tools so Claude CLI doesn't prompt for permission.
    // Skip for Bypass mode — bypassPermissions auto-approves everything, and adding
    // --allowedTools would accidentally RESTRICT tools to only those in the list.
    let mut allowed_tools = provider.allowed_tools.clone();
    if provider.permission_mode != "bypassPermissions" {
        for name in &mcp_server_names {
            allowed_tools.push(format!("mcp__{}__*", name));
        }
    }

    // Build final disallowed tools list: provider defaults + disabled MCP tools
    let mut disallowed_tools = provider.disallowed_tools.clone();
    if let Some(disabled) = &req.disabled_mcp_tools {
        // Map "serverId:toolName" → "mcp__serverName__toolName" (Claude CLI format)
        for entry in disabled {
            if let Some((server_id_str, tool_name)) = entry.split_once(':') {
                // Look up server name from MCP servers loaded earlier
                if let Ok(sid) = uuid::Uuid::parse_str(server_id_str)
                    && let Ok(name) = sqlx::query_scalar::<_, String>("SELECT name FROM mcp_servers WHERE id = $1")
                        .bind(sid)
                        .fetch_one(&state.pool)
                        .await
                {
                    disallowed_tools.push(format!("mcp__{}__{}", name, tool_name));
                }
            }
        }
    }

    // Prepare message persistence state shared across stream chunks
    // For resumed sessions, continue seq from where we left off
    let max_seq: i32 = if let Some(ref sid) = session_id {
        sqlx::query_scalar::<_, Option<i32>>("SELECT MAX(seq) FROM chat_messages WHERE session_id = $1")
            .bind(sid)
            .fetch_one(&pool)
            .await
            .ok()
            .flatten()
            .unwrap_or(0)
    } else {
        0
    };
    let seq = Arc::new(AtomicI32::new(max_seq));
    // Tracks the current assistant text message DB id (for appending chunks)
    let current_text_msg_id: Arc<tokio::sync::Mutex<Option<uuid::Uuid>>> = Arc::new(tokio::sync::Mutex::new(None));
    // Session ID discovered from Init chunk, shared with closure
    let stream_session_id: Arc<tokio::sync::Mutex<Option<String>>> =
        Arc::new(tokio::sync::Mutex::new(session_id.clone()));
    // Tracks the user message DB id for backfilling session_id after Init
    let user_msg_id: Arc<tokio::sync::Mutex<Option<uuid::Uuid>>> = Arc::new(tokio::sync::Mutex::new(None));

    // Save user message — session_id may be unknown yet, update later on Init
    let user_seq = seq.fetch_add(1, Ordering::Relaxed) + 1;
    let user_images_json = if req.images.is_empty() {
        None
    } else {
        Some(serde_json::to_value(&req.images).unwrap_or_default())
    };
    {
        let pool = pool.clone();
        let sid = session_id.clone().unwrap_or_default();
        let content = message_text.clone();
        let images_val = user_images_json.clone();
        let user_msg_id = user_msg_id.clone();
        tokio::spawn(async move {
            match ClaudeService::save_message(
                &pool,
                &sid,
                "user",
                &content,
                "text",
                None,
                images_val.as_ref(),
                None,
                user_seq,
            )
            .await
            {
                Ok(id) => {
                    *user_msg_id.lock().await = Some(id);
                }
                Err(e) => tracing::error!("Failed to save user message: {}", e),
            }
        });
    }

    // Create the agent
    let agent = crate::services::agent::claude::ClaudeAgent {
        bin_path: service.claude_bin.clone(),
        work_dir: service.work_dir.clone(),
        timeout: service.timeout,
    };

    // Build agent session config
    let agent_config = AgentSessionConfig {
        session_id: session_id.clone(),
        message: req.message.clone(),
        system_prompt: Some(system_prompt.clone()),
        model: service.model.clone(),
        max_turns: service.max_turns,
        permission_mode: provider.permission_mode.to_string(),
        allowed_tools: allowed_tools.clone(),
        disallowed_tools: disallowed_tools.clone(),
        env_vars: all_env_vars,
        mcp_config_path: mcp_config.clone(),
        images,
    };

    // Spawn Claude CLI process via Agent trait — skills are discovered via .claude/skills/ in user_work_dir
    let event_stream: SseEventStream = match agent.run(agent_config) {
        Ok(mut rx) => {
            let is_resume = session_id.is_some();
            let sse_stream = async_stream::stream! {
                let mut init_received = is_resume; // resume sessions skip --verbose, no init event expected
                let mut text_msg_started = false; // tracks whether current text message was already created (avoid wasting seq on appends)
                while let Some(event) = rx.recv().await {
                    // New sessions use --verbose which emits init before content.
                    // Wait for init to capture session_id; skip anything before it.
                    if !init_received {
                        if matches!(&event, AgentEvent::Init { .. }) {
                            init_received = true;
                        } else {
                            continue;
                        }
                    }

                    let chunk = StreamChunk::from_agent_event(&event);

                    // --- Persist session metadata ---
                    match &event {
                        AgentEvent::Init { session_id: Some(sid) } => {
                            let pool2 = pool.clone();
                            let sid2 = sid.clone();
                            let msg = message_text.clone();
                            tokio::spawn(async move {
                                let title = if msg.chars().count() > 50 {
                                    format!("{}...", msg.chars().take(50).collect::<String>())
                                } else {
                                    msg
                                };
                                if let Err(e) = ClaudeService::save_session(&pool2, &sid2, user_id, tenant_id, Some(&title)).await {
                                    tracing::error!("Failed to save session (init): {}", e);
                                }
                            });
                            // Store session_id and backfill user message with precise ID
                            let ssid = stream_session_id.clone();
                            let pool2 = pool.clone();
                            let sid2 = sid.clone();
                            let umid = user_msg_id.clone();
                            tokio::spawn(async move {
                                *ssid.lock().await = Some(sid2.clone());
                                if let Some(id) = *umid.lock().await {
                                    let _ = ClaudeService::backfill_message_session(&pool2, id, &sid2).await;
                                }
                            });
                        }
                        AgentEvent::Done { session_id: Some(sid), .. } => {
                            let pool2 = pool.clone();
                            let sid2 = sid.clone();
                            tokio::spawn(async move {
                                if let Err(e) = ClaudeService::save_session(&pool2, &sid2, user_id, tenant_id, None).await {
                                    tracing::error!("Failed to save session (done): {}", e);
                                }
                            });
                        }
                        _ => {}
                    }

                    // --- Persist message content ---
                    // seq is computed synchronously to preserve event order despite async spawning.
                    let spawn_save = |pool: sqlx::PgPool, ssid: Arc<tokio::sync::Mutex<Option<String>>>, s: i32, content: String, msg_type: &'static str, tool_name: Option<String>| {
                        tokio::spawn(async move {
                            let sid = ssid.lock().await.clone().unwrap_or_default();
                            if let Err(e) = ClaudeService::save_message(&pool, &sid, "assistant", &content, msg_type, tool_name.as_deref(), None, None, s).await {
                                tracing::error!("Failed to save {} message: {}", msg_type, e);
                            }
                        });
                    };

                    match &event {
                        AgentEvent::Text { content } => {
                            let pool2 = pool.clone();
                            let content = content.clone();
                            let text_id = current_text_msg_id.clone();
                            let ssid = stream_session_id.clone();
                            // Only allocate seq for the first text chunk (create); subsequent chunks append
                            let s = if !text_msg_started {
                                text_msg_started = true;
                                seq.fetch_add(1, Ordering::Relaxed) + 1
                            } else {
                                0 // unused — append path doesn't need seq
                            };
                            tokio::spawn(async move {
                                let existing = *text_id.lock().await;
                                if let Some(id) = existing {
                                    if let Err(e) = ClaudeService::append_message_content(&pool2, id, &content).await {
                                        tracing::error!("Failed to append text: {}", e);
                                    }
                                } else {
                                    let sid = ssid.lock().await.clone().unwrap_or_default();
                                    match ClaudeService::save_message(&pool2, &sid, "assistant", &content, "text", None, None, None, s).await {
                                        Ok(id) => { *text_id.lock().await = Some(id); }
                                        Err(e) => tracing::error!("Failed to save text message: {}", e),
                                    }
                                }
                            });
                        }
                        AgentEvent::Thinking { content } => {
                            let s = seq.fetch_add(1, Ordering::Relaxed) + 1;
                            spawn_save(pool.clone(), stream_session_id.clone(), s, content.clone(), "thinking", None);
                        }
                        AgentEvent::ToolUse { tool_name, content } => {
                            let s = seq.fetch_add(1, Ordering::Relaxed) + 1;
                            spawn_save(pool.clone(), stream_session_id.clone(), s, content.clone(), "tool_use", Some(tool_name.clone()));
                        }
                        AgentEvent::ToolResult { tool_name, content } => {
                            let s = seq.fetch_add(1, Ordering::Relaxed) + 1;
                            spawn_save(pool.clone(), stream_session_id.clone(), s, content.clone(), "tool_result", Some(tool_name.clone()));
                        }
                        AgentEvent::Done { duration_ms, .. } => {
                            text_msg_started = false; // reset for next turn's text
                            let text_id = current_text_msg_id.clone();
                            let dm = *duration_ms;
                            let pool2 = pool.clone();
                            tokio::spawn(async move {
                                if let Some(id) = *text_id.lock().await {
                                    let _ = ClaudeService::set_message_duration(&pool2, id, dm as i64).await;
                                }
                                *text_id.lock().await = None;
                            });
                        }
                        _ => {}
                    }

                    let data = serde_json::to_string(&chunk).unwrap_or_default();
                    yield Ok::<_, Infallible>(Event::default().data(data));
                }
            };
            Box::pin(sse_stream)
        }
        Err(e) => {
            tracing::error!("Failed to spawn Claude CLI: {}", e);
            let error_stream = futures::stream::once(async move {
                let chunk = StreamChunk::Error {
                    message: format!("Failed to start Claude: {}", e),
                };
                let data = serde_json::to_string(&chunk).unwrap_or_default();
                Ok::<_, Infallible>(Event::default().data(data))
            });
            Box::pin(error_stream)
        }
    };

    Sse::new(event_stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}

/// Format a regions list for CLAUDE.md display
fn fmt_regions(regions: &[String]) -> String {
    if regions.is_empty() {
        "ALL".to_string()
    } else {
        regions.join(", ")
    }
}

/// Generate a short-lived JWT for the agent to call Ops APIs
fn generate_agent_token(auth_user: &AuthUser, jwt_secret: &str) -> Option<String> {
    let now = chrono::Utc::now().timestamp() as usize;
    let claims = Claims {
        sub: auth_user.user_id,
        role: auth_user.role.clone(),
        tenant_id: auth_user.tenant_id,
        username: auth_user.username.clone(),
        token_type: "access".to_string(),
        iat: now,
        exp: now + 7200, // 2 hours — covers long-running agent sessions
    };
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .ok()
}

/// Build AWS credential environment variables for the Claude CLI subprocess.
/// Uses the management account (source='manual') as the hub — the agent can
/// then `aws sts assume-role` into child accounts as needed.
async fn build_aws_env_vars(state: &AppState, auth_user: &AuthUser) -> Vec<(String, String)> {
    let mut env_vars = Vec::new();

    let account_ids = crate::handlers::account_access::get_accessible_account_ids(&state.pool, auth_user).await;

    if account_ids.is_empty() {
        return env_vars;
    }

    // Find management account (manual source = hub that can assume-role into children).
    // Fallback to first available account for backward compatibility.
    let mgmt = sqlx::query_as::<_, (Option<String>, Option<String>, Vec<String>)>(
        r#"SELECT role_arn, profile, regions FROM cloud_accounts
           WHERE provider = 'aws' AND is_mock = false AND id = ANY($1)
           ORDER BY CASE WHEN source = 'manual' THEN 0 ELSE 1 END, created_at ASC
           LIMIT 1"#,
    )
    .bind(&account_ids)
    .fetch_optional(&state.pool)
    .await
    .ok()
    .flatten();

    if let Some((role_arn, profile, regions)) = mgmt {
        if let Some(arn) = role_arn
            && !arn.is_empty()
        {
            env_vars.push(("AWS_ROLE_ARN".to_string(), arn));
            env_vars.push(("AWS_ROLE_SESSION_NAME".to_string(), "opsk-chat".to_string()));
        }
        if let Some(prof) = profile
            && !prof.is_empty()
        {
            env_vars.push(("AWS_PROFILE".to_string(), prof));
        }
        if let Some(first_region) = regions.first() {
            env_vars.push(("AWS_DEFAULT_REGION".to_string(), first_region.clone()));
        }
    }

    if !env_vars.is_empty() {
        tracing::info!(
            "Injecting AWS env vars: {:?}",
            env_vars.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()
        );
    }

    env_vars
}

/// Build MCP config file for Claude CLI --mcp-config flag.
/// Queries enabled MCP servers, writes JSON to a temp file in user_work_dir,
/// and returns (file_path, server_names). Claude CLI expects a file path, not a JSON string.
/// When `server_ids` is Some, only include those specific servers.
async fn build_mcp_config(
    state: &AppState,
    auth_user: &AuthUser,
    user_work_dir: &std::path::Path,
    server_ids: Option<&[uuid::Uuid]>,
    api_token: Option<&str>,
) -> (Option<String>, Vec<String>) {
    let all_servers = sqlx::query_as::<_, crate::models::mcp::McpServer>(
        r#"SELECT * FROM mcp_servers
           WHERE enabled = true
           AND ((user_id = $1) OR (user_id IS NULL AND tenant_id IS NOT DISTINCT FROM $2))
           ORDER BY name"#,
    )
    .bind(auth_user.user_id)
    .bind(auth_user.tenant_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    // Filter by requested IDs if provided
    let servers: Vec<_> = match server_ids {
        Some(ids) if !ids.is_empty() => all_servers.into_iter().filter(|s| ids.contains(&s.id)).collect(),
        _ => all_servers,
    };

    if servers.is_empty() {
        return (None, Vec::new());
    }

    let server_names: Vec<String> = servers.iter().map(|s| s.name.clone()).collect();
    let mut mcp_servers = serde_json::Map::new();

    for srv in &servers {
        let entry = match srv.transport_type.as_str() {
            "sse" | "http" => {
                let mut obj = serde_json::Map::new();
                obj.insert("type".to_string(), serde_json::json!(srv.transport_type));
                if let Some(url) = &srv.url {
                    // Resolve relative paths (e.g. "/api/mcp/rollouts") to a full URL
                    // so Claude CLI can reach the backend from within the same pod.
                    let resolved = if url.starts_with('/') {
                        let base = std::env::var("SELF_BASE_URL")
                            .unwrap_or_else(|_| format!("http://localhost:{}", state.config.backend_port));
                        format!("{}{}", base.trim_end_matches('/'), url)
                    } else {
                        url.clone()
                    };
                    obj.insert("url".to_string(), serde_json::json!(resolved));
                }
                // For opsk-* built-in servers, auto-inject Authorization header
                let mut merged_headers = if srv.headers != serde_json::json!({}) {
                    srv.headers.clone()
                } else {
                    serde_json::json!({})
                };
                if srv.name.starts_with("opsk-")
                    && let Some(token) = api_token
                    && let Some(obj_map) = merged_headers.as_object_mut()
                {
                    obj_map.insert(
                        "Authorization".to_string(),
                        serde_json::json!(format!("Bearer {}", token)),
                    );
                }
                if merged_headers != serde_json::json!({}) {
                    obj.insert("headers".to_string(), merged_headers);
                }
                if srv.env != serde_json::json!({}) {
                    obj.insert("env".to_string(), srv.env.clone());
                }
                serde_json::Value::Object(obj)
            }
            _ => {
                // stdio
                let mut obj = serde_json::Map::new();
                obj.insert("command".to_string(), serde_json::json!(srv.command));
                if srv.args != serde_json::json!([]) {
                    obj.insert("args".to_string(), srv.args.clone());
                }
                if srv.env != serde_json::json!({}) {
                    obj.insert("env".to_string(), srv.env.clone());
                }
                serde_json::Value::Object(obj)
            }
        };
        mcp_servers.insert(srv.name.clone(), entry);
    }

    let config = serde_json::json!({ "mcpServers": mcp_servers });

    tracing::info!(
        "MCP config: {} server(s): {:?}",
        servers.len(),
        servers.iter().map(|s| &s.name).collect::<Vec<_>>()
    );

    // Write config to a file inside user_work_dir (works in local dev + cloud EKS pods)
    let config_path = user_work_dir.join(".mcp.json");
    if let Err(e) = tokio::fs::create_dir_all(user_work_dir).await {
        tracing::error!("Failed to create user work dir for MCP config: {}", e);
        return (None, server_names);
    }
    match tokio::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap_or_default()).await {
        Ok(_) => {
            tracing::info!("MCP config written to {}", config_path.display());
            // Return just the filename — Claude CLI runs with current_dir=user_work_dir
            (Some(".mcp.json".to_string()), server_names)
        }
        Err(e) => {
            tracing::error!("Failed to write MCP config file: {}", e);
            (None, server_names)
        }
    }
}

/// Build system prompt — lightweight since CLAUDE.md handles the heavy lifting.
/// System prompt is for basic role identity; CLAUDE.md (loaded natively by Claude CLI)
/// handles all detailed instructions, API endpoints, glossary, and accounts.
async fn build_system_prompt(
    _state: &AppState,
    _auth_user: &AuthUser,
    _user_work_dir: &std::path::Path,
    custom: Option<&str>,
) -> String {
    let mut parts = vec![
        "You are Ops AI, a multi-cloud infrastructure operations assistant.".to_string(),
        "Answer in the user's language. Be concise and actionable.".to_string(),
        "Follow all instructions in CLAUDE.md carefully.".to_string(),
    ];

    if let Some(custom) = custom {
        parts.push(format!("\n{}", custom));
    }

    parts.join("\n")
}

/// Write `ops-api` helper script into the workspace.
/// This avoids `$OPS_API_TOKEN` appearing in Bash commands, which triggers
/// Claude CLI's "Contains simple_expansion" security filter that hides tool output.
/// Usage: `ops-api GET /api/accounts` or `ops-api POST /api/jira/create '{"summary":"..."}'`
async fn write_ops_api_script(user_work_dir: &std::path::Path, token: &str, api_base: &str) {
    let script = format!(
        r#"#!/usr/bin/env bash
# ops-api — authenticated Ops API helper (auto-generated, do not edit)
# Usage: ops-api METHOD PATH [JSON_BODY]
# Examples:
#   ops-api GET /api/accounts
#   ops-api POST /api/jira/create '{{"summary":"test"}}'
set -euo pipefail
METHOD="${{1:?Usage: ops-api METHOD PATH [BODY]}}"
PATH_ARG="${{2:?Usage: ops-api METHOD PATH [BODY]}}"
BODY="${{3:-}}"
BASE="{api_base}"
TOKEN="{token}"

ARGS=(-sS -X "$METHOD" -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json")
if [ -n "$BODY" ]; then
  ARGS+=(-d "$BODY")
fi
exec curl "${{ARGS[@]}}" "$BASE$PATH_ARG"
"#
    );

    // Put inside .claude/bin/ so it stays hidden from Workspace panel
    // (.claude is filtered out by collect_workspace_files)
    let bin_dir = user_work_dir.join(".claude").join("bin");
    if let Err(e) = tokio::fs::create_dir_all(&bin_dir).await {
        tracing::warn!("Failed to create .claude/bin dir: {}", e);
        return;
    }
    let script_path = bin_dir.join("ops-api");
    if let Err(e) = tokio::fs::write(&script_path, &script).await {
        tracing::warn!("Failed to write ops-api script: {}", e);
        return;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).await;
    }
    tracing::info!("Wrote ops-api script to {:?}", script_path);
}

/// Write CLAUDE.md into the user's workspace directory.
/// Claude CLI natively loads CLAUDE.md as project-level instructions with high priority.
/// This is far more effective than --system-prompt for controlling agent behavior.
async fn write_user_claude_md(state: &AppState, auth_user: &AuthUser, user_work_dir: &std::path::Path) {
    let account_ids = crate::handlers::account_access::get_accessible_account_ids(&state.pool, auth_user).await;
    let workspace_path = std::fs::canonicalize(user_work_dir).unwrap_or_else(|_| user_work_dir.to_path_buf());

    // ─── Query MCP servers + tools for this user ────────────────────
    let mcp_servers: Vec<crate::models::mcp::McpServer> = sqlx::query_as(
        r#"SELECT * FROM mcp_servers
           WHERE enabled = true
           AND ((user_id = $1) OR (user_id IS NULL AND tenant_id IS NOT DISTINCT FROM $2))
           ORDER BY name"#,
    )
    .bind(auth_user.user_id)
    .bind(auth_user.tenant_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    let has_mcp_tools = mcp_servers
        .iter()
        .any(|s| s.tools.as_array().map(|a| !a.is_empty()).unwrap_or(false));

    let mut lines = vec![
        "# Ops Agent Instructions".to_string(),
        String::new(),
        "You are Ops AI, a multi-cloud infrastructure operations assistant.".to_string(),
        "Answer in the user's language. Be concise and actionable.".to_string(),
        String::new(),
    ];

    // ─── MCP Tool Reference ───
    if has_mcp_tools {
        lines.push("## MCP Tools Reference".to_string());
        lines.push(String::new());
        lines.push(
            "You have MCP tools available. Use them **only when the task matches what the tool does**.".to_string(),
        );
        lines.push("Do NOT call MCP tools speculatively or \"just to check\" — only when they directly serve the user's request.".to_string());
        lines.push(String::new());
        for srv in &mcp_servers {
            if let Some(tools) = srv.tools.as_array()
                && !tools.is_empty()
            {
                let desc = srv.description.as_deref().unwrap_or("");
                lines.push(format!(
                    "**Server: {}**{}",
                    srv.name,
                    if desc.is_empty() {
                        String::new()
                    } else {
                        format!(" — {}", desc)
                    }
                ));
                for tool in tools {
                    let tname = tool.get("name").and_then(|v| v.as_str()).unwrap_or("?");
                    let tdesc = tool.get("description").and_then(|v| v.as_str()).unwrap_or("");
                    lines.push(format!("- `mcp__{}__{}`: {}", srv.name, tname, tdesc));
                }
                lines.push(String::new());
            }
        }
        lines.push("**Rules:**".to_string());
        lines.push("1. Match the tool to the task: knowledge/document questions → GraphRAG tools; rollout management → rollout tools; etc.".to_string());
        lines.push("2. For **infrastructure operations** (create/delete/modify AWS resources), use Bash + AWS CLI + the Approval flow below. Do NOT use MCP tools for infra changes.".to_string());
        lines.push("3. MCP tools handle auth automatically — no tokens needed.".to_string());
        lines.push("4. When NO MCP tool matches, use Bash/curl.".to_string());
        lines.push(String::new());
    }

    // Environment rules
    lines.push("## Environment Rules".to_string());
    lines.push(String::new());
    lines.push(format!(
        "- **Workspace**: All output files MUST be saved to `{}`",
        workspace_path.display()
    ));
    lines.push("- **Credentials**: AWS credentials and the `.claude/bin/ops-api` helper are pre-configured. NEVER echo, print, or verify credentials — just use them directly. NEVER ask the user for credentials.".to_string());
    lines.push(
        "- **Scope**: When a task requires choosing regions, months, time ranges — always ASK the user first."
            .to_string(),
    );
    lines.push(String::new());

    // MCP / RAG image handling
    lines.push("## MCP & RAG Image Handling".to_string());
    lines.push(String::new());
    lines.push("When RAG tools (e.g. rag_tool) return content containing images in markdown format like `![IMAGE: description](https://...)`, you MUST:".to_string());
    lines.push(
        "1. **Include the original image URL** as-is in your response using markdown: `![description](url)`"
            .to_string(),
    );
    lines.push("2. **NEVER recreate diagrams** as mermaid/ASCII/text when the original image is available".to_string());
    lines.push("3. The frontend renders markdown images natively — just pass the URL through".to_string());
    lines.push(String::new());

    // Knowledge API
    lines.push("## How to Answer Knowledge Questions".to_string());
    lines.push(String::new());
    lines.push("When the user asks about **internal terminology, glossary, abbreviations, runbooks, knowledge base entries, cloud accounts, or security findings**, you MUST query the Ops API.".to_string());
    lines.push(String::new());
    lines.push("The knowledge is stored in a database, NOT in local files. Do NOT search or read local files for this information.".to_string());
    lines.push(String::new());
    if has_mcp_tools {
        lines.push("For knowledge/document queries, prefer the MCP tools listed above. Use these curl commands for operations not covered by MCP tools:".to_string());
    } else {
        lines.push("Use these commands (env vars are pre-set):".to_string());
    }
    lines.push(String::new());
    lines.push("**IMPORTANT**: Always use the `.claude/bin/ops-api` wrapper script instead of raw curl. It handles authentication automatically and avoids output being hidden by security filters.".to_string());
    lines.push("Usage: `.claude/bin/ops-api METHOD PATH [JSON_BODY]`".to_string());
    lines.push(String::new());
    lines.push("### Discovery & Assets".to_string());
    lines.push("```bash".to_string());
    lines.push("# Cloud accounts (provider, account_id, regions, role_arn)".to_string());
    lines.push(".claude/bin/ops-api GET /api/accounts".to_string());
    lines.push("# Kubernetes clusters (name, cloud, region, status, endpoint)".to_string());
    lines.push(".claude/bin/ops-api GET /api/clusters".to_string());
    lines.push("# Service topology (real-time Ingress→Service→Deployment/Rollout graph)".to_string());
    lines.push(".claude/bin/ops-api GET /api/topology".to_string());
    lines.push("# Security resources & findings".to_string());
    lines.push(".claude/bin/ops-api GET /api/resources".to_string());
    lines.push(".claude/bin/ops-api GET /api/resources/dashboard".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("### Deployments (Argo Rollouts)".to_string());
    lines.push("```bash".to_string());
    lines.push("# List rollouts on a cluster".to_string());
    lines.push(".claude/bin/ops-api GET /api/clusters/{cluster_id}/rollouts".to_string());
    lines.push("# Get rollout detail (canary steps, containers)".to_string());
    lines.push(".claude/bin/ops-api GET /api/clusters/{cluster_id}/rollouts/{ns}/{name}".to_string());
    lines.push("# Promote rollout (step or full)".to_string());
    lines.push(
        ".claude/bin/ops-api POST /api/clusters/{cluster_id}/rollouts/{ns}/{name}/promote '{\"full\":false}'"
            .to_string(),
    );
    lines.push("# Rollback rollout".to_string());
    lines.push(".claude/bin/ops-api POST /api/clusters/{cluster_id}/rollouts/{ns}/{name}/rollback".to_string());
    lines.push("# Change strategy (canary/blueGreen/rollingUpdate)".to_string());
    lines.push(".claude/bin/ops-api POST /api/clusters/{cluster_id}/rollouts/{ns}/{name}/strategy '{\"strategy\":\"canary\",\"canarySteps\":[{\"setWeight\":20},{\"pause\":{}},{\"setWeight\":50},{\"pause\":{\"duration\":\"60s\"}}]}'".to_string());
    lines.push("# Analysis runs for a rollout".to_string());
    lines.push(".claude/bin/ops-api GET /api/clusters/{cluster_id}/rollouts/{ns}/{name}/analysis".to_string());
    lines.push("# Deployment history (audit log)".to_string());
    lines.push(".claude/bin/ops-api GET /api/deployment-events".to_string());
    lines.push("# Filter by cluster: ?cluster_id=UUID  or by rollout: &namespace=X&rollout_name=Y".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("### Issues & Observability".to_string());
    lines.push("```bash".to_string());
    lines.push("# Active issues / alerts (filter: ?status=open&severity=critical&issue_type=security)".to_string());
    lines.push(".claude/bin/ops-api GET /api/issues".to_string());
    lines.push("# Pending issue count".to_string());
    lines.push(".claude/bin/ops-api GET /api/issues/count".to_string());
    lines.push("# Issue detail".to_string());
    lines.push(".claude/bin/ops-api GET /api/issues/{id}".to_string());
    lines.push("# Update issue (change status/severity/description)".to_string());
    lines.push(".claude/bin/ops-api PUT /api/issues/{id} '{\"status\":\"resolved\"}'".to_string());
    lines.push("# Start root cause analysis on an issue (streams SSE)".to_string());
    lines.push(".claude/bin/ops-api POST /api/issues/{id}/rca".to_string());
    lines.push("# Telemetry config (Grafana/Mimir/Loki/Tempo endpoints)".to_string());
    lines.push(".claude/bin/ops-api GET /api/telemetry".to_string());
    lines.push("# Dashboard stats".to_string());
    lines.push(".claude/bin/ops-api GET /api/dashboard/stats".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("### Approvals (Check Status)".to_string());
    lines.push("```bash".to_string());
    lines
        .push("# List all approvals (filter: ?status=pending|approved|executing|executed|failed|rejected)".to_string());
    lines.push(".claude/bin/ops-api GET /api/approvals".to_string());
    lines.push("# Pending approval count".to_string());
    lines.push(".claude/bin/ops-api GET /api/approvals/count".to_string());
    lines.push("```".to_string());
    lines.push(
        "Use this to check if your previously created approval has been reviewed, is executing, or has completed."
            .to_string(),
    );
    lines.push("The `execution_result` field contains `{\"success\":true,\"output\":\"...\"}` or `{\"success\":false,\"error\":\"...\"}`.".to_string());
    lines.push(String::new());
    lines.push("### Knowledge & Glossary".to_string());
    lines.push("```bash".to_string());
    lines.push("# Glossary (internal terminology)".to_string());
    lines.push(".claude/bin/ops-api GET /api/glossary".to_string());
    lines.push("# Knowledge base (runbooks, docs)".to_string());
    lines.push(".claude/bin/ops-api GET /api/knowledge".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("### Integrations".to_string());
    lines.push("```bash".to_string());
    lines.push("# Notification channels (Slack, webhook, etc.)".to_string());
    lines.push(".claude/bin/ops-api GET /api/channels".to_string());
    lines.push("# LLM providers".to_string());
    lines.push(".claude/bin/ops-api GET /api/providers".to_string());
    lines.push("# MCP servers".to_string());
    lines.push(".claude/bin/ops-api GET /api/mcp".to_string());
    lines.push("# Scheduled jobs".to_string());
    lines.push(".claude/bin/ops-api GET /api/scheduled-jobs".to_string());
    lines.push("# Pipeline repos (Git)".to_string());
    lines.push(".claude/bin/ops-api GET /api/pipeline/repos".to_string());
    lines.push("```".to_string());
    lines.push(String::new());
    lines.push("## API Usage Rules".to_string());
    lines.push(String::new());
    lines.push("- Always call the relevant API FIRST before answering.".to_string());
    lines.push("- **Empty result handling**: If an API returns `[]` or `null`, that is the definitive answer — the data does not exist. Report this to the user immediately. Do NOT retry the same endpoint with different parameters, do NOT try alternative query approaches, do NOT loop. An empty array means zero records, not an error.".to_string());
    lines.push("- **Error handling**: Only retry on HTTP 5xx errors (max 1 retry). For 4xx errors, report the error to the user.".to_string());
    lines.push(String::new());

    // ─── Jira integration instructions (only if tenant has enabled Jira channel) ──
    let has_jira = if auth_user.tenant_id.is_some() {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM channels c JOIN channel_tenants ct ON ct.channel_id = c.id WHERE c.platform = 'jira' AND c.enabled = true AND ct.tenant_id = $1)",
        )
        .bind(auth_user.tenant_id)
        .fetch_one(&state.pool)
        .await
        .unwrap_or(false)
    } else {
        // Super admin: any enabled Jira channel
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM channels WHERE platform = 'jira' AND enabled = true)",
        )
        .fetch_one(&state.pool)
        .await
        .unwrap_or(false)
    };

    // ─── Approval flow instructions (always present) ─────────────────────────
    lines.push("### Approval Flow (CRITICAL — must follow for infrastructure changes)".to_string());
    lines.push(String::new());
    lines.push("When the user requests **infrastructure changes** (create/delete/modify cloud resources, security fixes, config changes), follow these steps **exactly**. Each step involves a real Bash tool call — **never skip or simulate** the commands.".to_string());
    lines.push(String::new());
    lines.push("**Step 1:** Describe to the user what you plan to do (resource, region, account, config).".to_string());
    lines.push(String::new());
    lines.push("**Step 2:** Run this in **Bash** using the `ops-api` helper:".to_string());
    lines.push("```bash".to_string());
    lines.push(".claude/bin/ops-api POST /api/approvals '{\"command\":\"<short description>\",\"reason\":\"<why>\",\"plan_detail\":{\"prompt\":\"<detailed execution prompt>\"}}'".to_string());
    lines.push("```".to_string());
    lines.push(
        "Read the JSON response. Extract the `id` field (a UUID like `550e8400-e29b-41d4-a716-446655440000`)."
            .to_string(),
    );
    lines.push(
        "**If the command fails or returns no `id`, tell the user the exact error.** Do not proceed.".to_string(),
    );
    lines.push(String::new());

    // Jira ticket is auto-created by the backend if a Jira channel is configured.
    // The response will include jira_key if a ticket was created.
    lines.push("**Step 3:** Report to the user with the **actual values** from the Step 2 response:".to_string());
    lines.push("- Approval ID: `<the id field>`".to_string());
    if has_jira {
        lines.push("- Jira ticket: `<jira_key field>` (auto-created by the system)".to_string());
    }
    lines.push(
        "- Tell them an admin can approve it in the Ops Approvals page, and it will auto-execute once approved."
            .to_string(),
    );

    lines.push(String::new());
    lines.push("**RULES:**".to_string());
    lines.push(
        "- Never fabricate an approval ID or Jira key. Only report values you received from the API.".to_string(),
    );
    lines.push(
        "- Never say \"approval created\" without having actually run the command and gotten a response.".to_string(),
    );
    lines.push("- Do NOT execute the infrastructure change yourself — only create the approval record.".to_string());
    lines.push("- Only create approvals for **write operations** (create/delete/modify). Read-only queries need no approval — just answer directly.".to_string());
    lines.push(String::new());

    // Inject glossary inline as quick reference (so agent doesn't need API call for common terms)
    if let Ok(terms) = sqlx::query_as::<_, (String, Option<String>, Option<String>)>(
        "SELECT term, full_name, description FROM glossary WHERE account_id = ANY($1) OR account_id IS NULL LIMIT 50",
    )
    .bind(&account_ids)
    .fetch_all(&state.pool)
    .await
        && !terms.is_empty()
    {
        lines.push("## Quick Glossary Reference".to_string());
        lines.push(String::new());
        for (term, full_name, desc) in terms {
            let full = full_name.unwrap_or_default();
            let d = desc.unwrap_or_default();
            lines.push(format!("- **{}** ({}): {}", term, full, d));
        }
        lines.push(String::new());
    }

    // Inject cloud accounts with multi-account switching instructions
    if let Ok(accounts) = sqlx::query_as::<_, (String, String, Option<String>, Option<String>, Vec<String>, String)>(
        "SELECT provider, name, account_id, role_arn, regions, source FROM cloud_accounts WHERE id = ANY($1) AND is_mock = false ORDER BY CASE WHEN source = 'manual' THEN 0 ELSE 1 END, created_at ASC LIMIT 20",
    )
    .bind(&account_ids)
    .fetch_all(&state.pool)
    .await
        && !accounts.is_empty()
    {
        let aws_accounts: Vec<_> = accounts.iter().filter(|a| a.0 == "aws").collect();
        let non_aws_accounts: Vec<_> = accounts.iter().filter(|a| a.0 != "aws").collect();

        if aws_accounts.len() > 1 {
            // Multi-account mode: hub-and-spoke with assume-role instructions
            lines.push("## AWS Multi-Account Access".to_string());
            lines.push(String::new());
            lines.push("Your base credentials authenticate as the management account (hub). To operate in a child account, assume the target role first.".to_string());
            lines.push(String::new());

            // Management account (first manual source)
            if let Some(mgmt) = aws_accounts.first() {
                let aid = mgmt.2.as_deref().unwrap_or("-");
                lines.push("### Management Account (Hub) — Already Active".to_string());
                lines.push(format!("- **{}** — Account: `{}`, Regions: [{}]", mgmt.1, aid, fmt_regions(&mgmt.4)));
                lines.push("- Do NOT create resources here unless the user explicitly names this account.".to_string());
                lines.push(String::new());
            }

            // Child accounts
            let children: Vec<_> = aws_accounts.iter().skip(1).collect();
            if !children.is_empty() {
                lines.push("### Child Accounts".to_string());
                for (i, acct) in children.iter().enumerate() {
                    let aid = acct.2.as_deref().unwrap_or("-");
                    let role = acct.3.as_deref().unwrap_or("N/A");
                    lines.push(format!("{}. **{}** — Account: `{}`, Regions: [{}], Role: `{}`", i + 1, acct.1, aid, fmt_regions(&acct.4), role));
                }
                lines.push(String::new());
            }

            // Assume-role instructions
            lines.push("### How to Switch Accounts".to_string());
            lines.push(String::new());
            lines.push("```bash".to_string());
            lines.push("# Assume target account's role".to_string());
            lines.push("CREDS=$(aws sts assume-role \\".to_string());
            lines.push("  --role-arn \"TARGET_ROLE_ARN\" \\".to_string());
            lines.push("  --role-session-name \"opsk-agent\" \\".to_string());
            lines.push("  --query 'Credentials.[AccessKeyId,SecretAccessKey,SessionToken]' \\".to_string());
            lines.push("  --output text)".to_string());
            lines.push("export AWS_ACCESS_KEY_ID=$(echo $CREDS | awk '{print $1}')".to_string());
            lines.push("export AWS_SECRET_ACCESS_KEY=$(echo $CREDS | awk '{print $2}')".to_string());
            lines.push("export AWS_SESSION_TOKEN=$(echo $CREDS | awk '{print $3}')".to_string());
            lines.push("aws sts get-caller-identity  # verify".to_string());
            lines.push("```".to_string());
            lines.push(String::new());
            lines.push("**Rules:**".to_string());
            lines.push("- Always `aws sts get-caller-identity` before creating resources.".to_string());
            lines.push("- To switch back to management: `unset AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN`".to_string());
            lines.push("- To switch to another child: unset first, then assume the new role.".to_string());
            lines.push("- Match user's account name (e.g. \"Production\", \"Staging\") to the list above.".to_string());
            lines.push("- Append `--region <region>` to target a specific region.".to_string());
            lines.push(String::new());
        } else {
            // Single AWS account or no AWS accounts — simple listing
            for (provider, name, account_id, role_arn, regions, _source) in &aws_accounts {
                let aid = account_id.as_deref().unwrap_or("-");
                let role_info = role_arn.as_deref().map(|r| format!(", Role: {r}")).unwrap_or_default();
                lines.push(format!("- {} ({}) — Account: {}, Regions: [{}]{}", name, provider, aid, fmt_regions(regions), role_info));
            }
            if !aws_accounts.is_empty() {
                lines.push(String::new());
            }
        }

        // Non-AWS accounts (Azure, GCP, etc.) — simple listing
        if !non_aws_accounts.is_empty() {
            if aws_accounts.is_empty() {
                lines.push("## Available Cloud Accounts".to_string());
                lines.push(String::new());
            }
            for (provider, name, account_id, _role_arn, regions, _source) in &non_aws_accounts {
                let aid = account_id.as_deref().unwrap_or("-");
                lines.push(format!("- {} ({}) — Account: {}, Regions: [{}]", name, provider, aid, fmt_regions(regions)));
            }
            lines.push(String::new());
        }
    }

    // Inject clusters
    if let Ok(clusters) = sqlx::query_as::<_, (String, String, String, Option<String>, Option<String>, String)>(
        "SELECT name, cloud, cluster_type, region, account_id, status FROM clusters WHERE tenant_id IS NOT DISTINCT FROM $1 LIMIT 20",
    )
    .bind(auth_user.tenant_id)
    .fetch_all(&state.pool)
    .await
        && !clusters.is_empty()
    {
        lines.push("## Kubernetes Clusters".to_string());
        lines.push(String::new());
        for (name, cloud, ctype, region, account_id, status) in &clusters {
            let r = region.as_deref().unwrap_or("?");
            let aid = account_id.as_deref().unwrap_or("-");
            lines.push(format!("- {} ({}/{}) — Region: {}, Account: {}, Status: {}", name, cloud, ctype, r, aid, status));
        }
        lines.push(String::new());
    }

    // ─── Observability note ──────────────────────────────────────
    // Full prediction + RCA instructions are in project CLAUDE.md.
    // Here we just remind the agent how to discover telemetry endpoints at runtime.
    lines.push("## Observability".to_string());
    lines.push(String::new());
    lines.push(
        "Prediction and RCA instructions are defined in the project CLAUDE.md. To get live telemetry endpoints:"
            .to_string(),
    );
    lines.push(String::new());
    lines.push("```bash".to_string());
    lines.push("# Fetch configured telemetry provider + endpoints".to_string());
    lines.push(".claude/bin/ops-api GET /api/telemetry".to_string());
    lines.push("```".to_string());
    lines.push(String::new());

    let content = lines.join("\n");
    let claude_md_path = user_work_dir.join("CLAUDE.md");

    // Only write if content changed (avoid unnecessary disk writes)
    let should_write = match tokio::fs::read_to_string(&claude_md_path).await {
        Ok(existing) => existing != content,
        Err(_) => true,
    };

    if should_write {
        // Ensure directory exists
        if let Err(e) = tokio::fs::create_dir_all(user_work_dir).await {
            tracing::warn!("Failed to create user work dir {:?}: {}", user_work_dir, e);
            return;
        }
        if let Err(e) = tokio::fs::write(&claude_md_path, &content).await {
            tracing::warn!("Failed to write CLAUDE.md to {:?}: {}", claude_md_path, e);
        } else {
            tracing::info!("Wrote CLAUDE.md ({} bytes) to {:?}", content.len(), claude_md_path);
        }
    }
}

/// Write PreToolUse hooks to enforce the approval flow for infrastructure changes.
/// This is a hard constraint — the hook script runs before every Bash tool invocation
/// and blocks AWS write commands (create/delete/modify/etc.) with exit code 2.
/// The agent is forced to use the approval API instead of executing commands directly.
async fn write_approval_hooks(user_work_dir: &std::path::Path) {
    let claude_dir = user_work_dir.join(".claude");
    let hooks_dir = claude_dir.join("hooks");

    // Hook content is static — skip if already written
    let settings_path = claude_dir.join("settings.local.json");
    if settings_path.exists() && hooks_dir.join("enforce-approval.sh").exists() {
        return;
    }

    if let Err(e) = tokio::fs::create_dir_all(&hooks_dir).await {
        tracing::warn!("Failed to create .claude/hooks dir: {}", e);
        return;
    }

    // Hook script: intercept Bash tool, block AWS write operations
    let hook_script = r#"#!/usr/bin/env bash
# PreToolUse hook: enforce approval flow for AWS write operations.
# Exit 0 = allow, Exit 2 = block (stdout shown to agent as reason).
set -euo pipefail
INPUT=$(cat)
TOOL=$(echo "$INPUT" | jq -r '.tool_name // empty' 2>/dev/null)
[[ "$TOOL" != "Bash" ]] && exit 0

CMD=$(echo "$INPUT" | jq -r '.tool_input.command // empty' 2>/dev/null)
[[ -z "$CMD" ]] && exit 0

# AWS write verbs — block direct execution
if echo "$CMD" | grep -qiE 'aws\s+\S+\s+(create-|delete-|modify-|put-|run-instances|terminate-|update-|start-db|stop-db|reboot-db|allocate-|release-|attach-|detach-|authorize-|revoke-|register-|deregister-)'; then
  cat <<'REASON'
BLOCKED: AWS write operations must go through the Approval Flow.
Follow the steps in CLAUDE.md:
1. .claude/bin/ops-api POST /api/approvals '{"command":"...","reason":"...","plan_detail":{"prompt":"..."}}' to create an approval (Jira ticket is auto-created)
2. Report the approval ID and Jira key to the user
3. DO NOT execute the infrastructure command directly
REASON
  exit 2
fi

# terraform apply/destroy — also require approval
if echo "$CMD" | grep -qiE 'terraform\s+(apply|destroy)'; then
  echo "BLOCKED: Terraform apply/destroy requires approval. Use the Approval Flow in CLAUDE.md."
  exit 2
fi

exit 0
"#;

    let hook_path = hooks_dir.join("enforce-approval.sh");

    // Write hook script
    if let Err(e) = tokio::fs::write(&hook_path, hook_script).await {
        tracing::warn!("Failed to write hook script: {}", e);
        return;
    }

    // Make executable
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = tokio::fs::set_permissions(&hook_path, std::fs::Permissions::from_mode(0o755)).await;
    }

    // Write .claude/settings.local.json with hook config
    let hook_path_str = hook_path.to_string_lossy();
    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "Bash",
                "hooks": [{
                    "type": "command",
                    "command": hook_path_str
                }]
            }]
        }
    });

    let settings_path = claude_dir.join("settings.local.json");
    match serde_json::to_string_pretty(&settings) {
        Ok(content) => {
            if let Err(e) = tokio::fs::write(&settings_path, content).await {
                tracing::warn!("Failed to write settings.local.json: {}", e);
            } else {
                tracing::info!("Wrote approval enforcement hooks to {:?}", settings_path);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize hook settings: {}", e),
    }
}

/// Build per-user `.claude/skills/` directory with symlinks to authorized skills only.
/// This ensures Claude CLI's native skill discovery (`/skill-name`) only sees
/// skills the user has permission to access (private + tenant-public).
async fn setup_user_skill_symlinks(
    state: &AppState,
    user_work_dir: &std::path::Path,
    user_id: uuid::Uuid,
    tenant_id: Option<uuid::Uuid>,
) {
    let skills_link_dir = user_work_dir.join(".claude").join("skills");

    // Create the directory structure
    if let Err(e) = tokio::fs::create_dir_all(&skills_link_dir).await {
        tracing::warn!("Failed to create user skills dir {:?}: {}", skills_link_dir, e);
        return;
    }

    // Query authorized skills with layer override: private (user_id) takes priority
    // over public (user_id IS NULL) for same name. DISTINCT ON + ORDER BY ensures
    // the private version wins when both exist.
    let authorized: Vec<(String, Option<String>)> = sqlx::query_as(
        r#"SELECT DISTINCT ON (name) name, repo_path FROM skills
           WHERE enabled = true AND repo_path IS NOT NULL
             AND ((user_id = $1) OR (user_id IS NULL AND tenant_id IS NOT DISTINCT FROM $2))
           ORDER BY name, user_id NULLS LAST"#,
    )
    .bind(user_id)
    .bind(tenant_id)
    .fetch_all(&state.pool)
    .await
    .unwrap_or_default();

    tracing::info!(
        ?user_id,
        ?tenant_id,
        count = authorized.len(),
        skills = ?authorized.iter().map(|(n, p)| format!("{}: {:?}", n, p)).collect::<Vec<_>>(),
        "setup_user_skill_symlinks: queried authorized skills"
    );

    // Collect authorized skill dir names
    let authorized_names: std::collections::HashSet<String> = authorized
        .iter()
        .filter_map(|(_, rp)| {
            rp.as_ref().and_then(|p| {
                std::path::Path::new(p)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|s| s.to_string())
            })
        })
        .collect();

    // Remove stale symlinks (skills user no longer has access to)
    if let Ok(mut entries) = tokio::fs::read_dir(&skills_link_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if !authorized_names.contains(&name) {
                let _ = tokio::fs::remove_file(entry.path()).await;
                let _ = tokio::fs::remove_dir_all(entry.path()).await;
            }
        }
    }

    // Create/update symlinks for authorized skills
    for (skill_name, repo_path) in &authorized {
        if let Some(rp) = repo_path {
            let src = std::path::Path::new(rp);

            // Claude CLI requires uppercase SKILL.md
            if src.exists() {
                crate::services::skill::normalize_skill_md_case(src).await;
            }

            if let Some(dir_name) = src.file_name() {
                let link = skills_link_dir.join(dir_name);
                // Skip if symlink already points to the right place
                if let Ok(target) = tokio::fs::read_link(&link).await {
                    if target == src {
                        tracing::debug!(skill = %skill_name, "Skill symlink already correct");
                        continue;
                    }
                    // Stale symlink, remove
                    let _ = tokio::fs::remove_file(&link).await;
                }
                let exists = src.exists();
                if exists {
                    match tokio::fs::symlink(src, &link).await {
                        Ok(()) => tracing::info!(skill = %skill_name, src = %rp, link = ?link, "Created skill symlink"),
                        Err(e) => {
                            tracing::warn!(skill = %skill_name, src = %rp, link = ?link, "Failed to symlink skill: {}", e)
                        }
                    }
                } else {
                    tracing::warn!(skill = %skill_name, src = %rp, "Skill repo_path does not exist on disk, skipping symlink");
                }
            }
        }
    }
}

/// GET /api/chat/sessions — list user's chat sessions
pub async fn list_sessions(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> crate::error::AppResult<Json<Vec<ChatSession>>> {
    let sessions = sqlx::query_as::<_, ChatSession>(
        r#"SELECT id, claude_session_id, title, last_message, is_active, created_at, last_active_at
           FROM claude_sessions
           WHERE user_id = $1
           AND ($2::UUID IS NULL OR tenant_id = $2)
           AND last_active_at > NOW() - INTERVAL '24 hours'
           ORDER BY last_active_at DESC
           LIMIT 20"#,
    )
    .bind(auth_user.user_id)
    .bind(auth_user.tenant_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(sessions))
}

/// GET /api/chat/sessions/:session_id/messages — load message history for a session
pub async fn get_messages(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(session_id): Path<String>,
) -> AppResult<Json<Vec<ChatMessageRow>>> {
    // Verify the session belongs to this user
    let exists = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM claude_sessions WHERE claude_session_id = $1 AND user_id = $2)",
    )
    .bind(&session_id)
    .bind(auth_user.user_id)
    .fetch_one(&state.pool)
    .await?;

    if !exists {
        return Err(AppError::NotFound("Session not found".into()));
    }

    let rows = sqlx::query_as::<_, ChatMessageRow>(
        r#"SELECT id, session_id, role, content, msg_type, tool_name, images, duration_ms, seq, created_at
           FROM chat_messages
           WHERE session_id = $1
           ORDER BY seq ASC, created_at ASC"#,
    )
    .bind(&session_id)
    .fetch_all(&state.pool)
    .await?;

    Ok(Json(rows))
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct ChatMessageRow {
    pub id: uuid::Uuid,
    pub session_id: String,
    pub role: String,
    pub content: String,
    pub msg_type: String,
    pub tool_name: Option<String>,
    pub images: Option<serde_json::Value>,
    pub duration_ms: Option<i64>,
    pub seq: i32,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

// Re-export shared helper
use crate::services::claude::build_provider_env_vars;

const DEFAULT_DISALLOWED_TOOLS: &[&str] = &["Write", "Edit", "NotebookEdit"];
const DEFAULT_ALLOWED_TOOLS: &[&str] = &["Bash"];
/// All Claude CLI tools — used for ReadWrite mode where we need to pass --allowedTools
/// but don't want to restrict which tools are available.
const ALL_STANDARD_TOOLS: &[&str] = &[
    "Bash",
    "Read",
    "Write",
    "Edit",
    "Glob",
    "Grep",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
];

/// Provider configuration extracted from DB
struct ProviderSettings {
    model: String,
    timeout: Duration,
    max_turns: u32,
    env_vars: Vec<(String, String)>,
    permission_mode: &'static str,
    disallowed_tools: Vec<String>,
    allowed_tools: Vec<String>,
}

/// Load model + timeout + max_turns + provider env vars + permission settings from providers table.
/// If provider_id is given, use that specific provider; otherwise use the tenant's default.
async fn load_provider_config(
    state: &AppState,
    tenant_id: Option<uuid::Uuid>,
    provider_id: Option<uuid::Uuid>,
) -> ProviderSettings {
    let row = if let Some(pid) = provider_id {
        if tenant_id.is_some() {
            // Verify provider is assigned to tenant
            sqlx::query_as::<_, (String, serde_json::Value)>(
                r#"SELECT p.provider_type, p.config FROM providers p
                   JOIN tenant_providers tp ON p.id = tp.provider_id
                   WHERE p.id = $1 AND tp.tenant_id = $2"#,
            )
            .bind(pid)
            .bind(tenant_id)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
        } else {
            // super_admin: direct lookup
            sqlx::query_as::<_, (String, serde_json::Value)>(
                "SELECT provider_type, config FROM providers WHERE id = $1",
            )
            .bind(pid)
            .fetch_optional(&state.pool)
            .await
            .ok()
            .flatten()
        }
    } else if let Some(tid) = tenant_id {
        // Tenant default provider
        sqlx::query_as::<_, (String, serde_json::Value)>(
            r#"SELECT p.provider_type, p.config FROM providers p
               JOIN tenant_providers tp ON p.id = tp.provider_id
               WHERE tp.tenant_id = $1 AND tp.is_default = true
               LIMIT 1"#,
        )
        .bind(tid)
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
    } else {
        // super_admin with no tenant: use first provider
        sqlx::query_as::<_, (String, serde_json::Value)>(
            "SELECT provider_type, config FROM providers ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(&state.pool)
        .await
        .ok()
        .flatten()
    };

    if let Some((provider_type, config)) = row {
        let model = config
            .get("model")
            .and_then(|v| v.as_str())
            .unwrap_or(&state.config.claude_model)
            .to_string();
        let timeout_ms = config
            .get("timeout")
            .and_then(|v| v.as_u64())
            .unwrap_or(state.config.claude_timeout_ms);
        let max_turns = config.get("max_turns").and_then(|v| v.as_u64()).unwrap_or(25) as u32;
        let provider_envs = build_provider_env_vars(&provider_type, &config);

        let perm = AgentPermission::from_config(
            config
                .get("permission_mode")
                .and_then(|v| v.as_str())
                .unwrap_or("readonly"),
        );

        let (disallowed_tools, allowed_tools) = match perm {
            AgentPermission::Readonly => {
                let disallowed = config
                    .get("disallowed_tools")
                    .and_then(|v| v.as_array())
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_else(|| DEFAULT_DISALLOWED_TOOLS.iter().map(|s| s.to_string()).collect());
                let allowed = DEFAULT_ALLOWED_TOOLS.iter().map(|s| s.to_string()).collect();
                (disallowed, allowed)
            }
            AgentPermission::ReadWrite => {
                // ReadWrite needs all standard tools pre-approved (--permission-mode default
                // requires interactive approval, but we run non-interactively as a subprocess).
                let allowed = ALL_STANDARD_TOOLS.iter().map(|s| s.to_string()).collect();
                (Vec::new(), allowed)
            }
            AgentPermission::Bypass => {
                // bypassPermissions auto-approves everything — don't pass --allowedTools
                // to avoid accidentally restricting available tools.
                (Vec::new(), Vec::new())
            }
        };

        ProviderSettings {
            model,
            timeout: Duration::from_millis(timeout_ms),
            max_turns,
            env_vars: provider_envs,
            permission_mode: perm.cli_flag(),
            disallowed_tools,
            allowed_tools,
        }
    } else {
        ProviderSettings {
            model: state.config.claude_model.clone(),
            timeout: Duration::from_millis(state.config.claude_timeout_ms),
            max_turns: 25,
            env_vars: Vec::new(),
            permission_mode: AgentPermission::Readonly.cli_flag(),
            disallowed_tools: DEFAULT_DISALLOWED_TOOLS.iter().map(|s| s.to_string()).collect(),
            allowed_tools: DEFAULT_ALLOWED_TOOLS.iter().map(|s| s.to_string()).collect(),
        }
    }
}

#[derive(Debug, serde::Serialize, sqlx::FromRow)]
pub struct ChatSession {
    pub id: uuid::Uuid,
    pub claude_session_id: String,
    pub title: Option<String>,
    pub last_message: Option<String>,
    pub is_active: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_active_at: chrono::DateTime<chrono::Utc>,
}

// ─── Workspace ──────────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
pub struct WorkspaceFile {
    pub name: String,
    pub size: u64,
    pub modified: String,
    pub is_dir: bool,
}

/// GET /api/chat/workspace — list files in user's workspace (recursive)
pub async fn workspace_list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<Vec<WorkspaceFile>>> {
    let base = PathBuf::from(&state.config.claude_work_dir);
    let user_dir = base.join("users").join(auth_user.user_id.to_string());
    let scans_dir = base.join("scans");

    let mut files = Vec::new();
    // User-specific workspace files
    collect_workspace_files(&user_dir, &user_dir, &mut files).await;
    // Shared scan reports (prefix paths with "scans/")
    let mut scan_files = Vec::new();
    collect_workspace_files(&scans_dir, &scans_dir, &mut scan_files).await;
    for mut f in scan_files {
        f.name = format!("scans/{}", f.name);
        files.push(f);
    }
    files.sort_by(|a, b| b.modified.cmp(&a.modified));
    Ok(Json(files))
}

/// Recursively collect files from workspace, using relative paths from root
async fn collect_workspace_files(root: &std::path::Path, dir: &std::path::Path, files: &mut Vec<WorkspaceFile>) {
    let Ok(mut entries) = tokio::fs::read_dir(dir).await else {
        return;
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden dirs (.claude, .git, etc.) and internal tool files
        if name.starts_with('.') || name == "CLAUDE.md" || name == "ops-api" {
            continue;
        }
        let path = entry.path();
        if let Ok(meta) = entry.metadata().await {
            let rel_path = path
                .strip_prefix(root)
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or(name.clone());
            if meta.is_dir() {
                Box::pin(collect_workspace_files(root, &path, files)).await;
            } else {
                files.push(WorkspaceFile {
                    name: rel_path,
                    size: meta.len(),
                    modified: chrono::DateTime::<chrono::Utc>::from(
                        meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH),
                    )
                    .format("%Y-%m-%d %H:%M")
                    .to_string(),
                    is_dir: false,
                });
            }
        }
    }
}

/// GET /api/chat/workspace/*filepath — download a file from workspace
pub async fn workspace_download(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> Result<axum::response::Response, AppError> {
    use axum::response::IntoResponse;

    // Sanitize — no path traversal
    if filename.contains("..") || filename.contains('\\') {
        return Err(AppError::BadRequest("Invalid filename".to_string()));
    }

    let base = PathBuf::from(&state.config.claude_work_dir);

    // Support both user files and shared scan reports
    let (file_path, allowed_root) = if filename.starts_with("scans/") {
        (base.join(&filename), base.join("scans"))
    } else {
        let user_dir = base.join("users").join(auth_user.user_id.to_string());
        (user_dir.join(&filename), user_dir)
    };

    // Ensure resolved path is still under allowed root (prevent symlink escape)
    if let (Ok(resolved), Ok(root_resolved)) = (file_path.canonicalize(), allowed_root.canonicalize())
        && !resolved.starts_with(&root_resolved)
    {
        return Err(AppError::BadRequest("Invalid path".to_string()));
    }

    if !file_path.exists() || file_path.is_dir() {
        return Err(AppError::NotFound("File not found".to_string()));
    }

    let bytes = tokio::fs::read(&file_path)
        .await
        .map_err(|e| AppError::Internal(format!("Failed to read file: {}", e)))?;

    let content_type = if filename.ends_with(".xlsx") {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    } else if filename.ends_with(".csv") {
        "text/csv"
    } else if filename.ends_with(".json") {
        "application/json"
    } else if filename.ends_with(".pdf") {
        "application/pdf"
    } else {
        "application/octet-stream"
    };

    Ok((
        [
            (http::header::CONTENT_TYPE, content_type),
            (
                http::header::CONTENT_DISPOSITION,
                &format!(
                    "attachment; filename=\"{}\"",
                    filename.split('/').next_back().unwrap_or(&filename)
                ),
            ),
        ],
        bytes,
    )
        .into_response())
}

/// DELETE /api/chat/workspace/*filepath — delete a file from workspace
pub async fn workspace_delete(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(filename): Path<String>,
) -> AppResult<Json<serde_json::Value>> {
    // Sanitize — no path traversal
    if filename.contains("..") || filename.contains('\\') {
        return Err(AppError::BadRequest("Invalid filename".to_string()));
    }

    let base = PathBuf::from(&state.config.claude_work_dir);

    let (file_path, allowed_root) = if filename.starts_with("scans/") {
        (base.join(&filename), base.join("scans"))
    } else {
        let user_dir = base.join("users").join(auth_user.user_id.to_string());
        (user_dir.join(&filename), user_dir)
    };

    // Ensure resolved path is still under allowed root
    if let (Ok(resolved), Ok(root_resolved)) = (file_path.canonicalize(), allowed_root.canonicalize())
        && !resolved.starts_with(&root_resolved)
    {
        return Err(AppError::BadRequest("Invalid path".to_string()));
    }

    if !file_path.exists() {
        return Err(AppError::NotFound("File not found".to_string()));
    }

    if file_path.is_dir() {
        tokio::fs::remove_dir_all(&file_path)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to delete directory: {}", e)))?;
    } else {
        tokio::fs::remove_file(&file_path)
            .await
            .map_err(|e| AppError::Internal(format!("Failed to delete file: {}", e)))?;
    }

    // Clean up empty parent dirs (up to user_dir)
    let mut current = file_path.parent().map(|p| p.to_path_buf());
    let user_dir_resolved = allowed_root.canonicalize().unwrap_or(allowed_root.clone());
    while let Some(p) = current {
        let p_resolved = p.canonicalize().unwrap_or(p.clone());
        if p_resolved == user_dir_resolved {
            break;
        }
        // Try to remove — only succeeds if empty
        if tokio::fs::remove_dir(&p).await.is_err() {
            break;
        }
        current = p.parent().map(|pp| pp.to_path_buf());
    }

    Ok(Json(serde_json::json!({"message": "Deleted"})))
}
