use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regex::Regex;
use sqlx::PgPool;
use tokio::sync::{Mutex, broadcast};
use uuid::Uuid;

use crate::config::AppConfig;
use crate::models::issue::Issue;
use crate::services::agent::{Agent, AgentEvent, AgentSessionConfig};
use crate::services::claude::StreamChunk;

struct StepRecord {
    step_id: String,
    tool_name: String,
    reasoning: String,
    label: String,
    analysis: String,
    data_text: String,
}

impl StepRecord {
    fn into_json(self, duration_ms: u64) -> serde_json::Value {
        serde_json::json!({
            "stepId": self.step_id,
            "toolName": self.tool_name,
            "reasoning": self.reasoning,
            "label": self.label,
            "status": "complete",
            "analysis": self.analysis,
            "dataText": self.data_text,
            "durationMs": duration_ms
        })
    }
}

/// In-memory registry of currently-running RCA analyses.
pub struct RcaRegistry {
    active: Mutex<HashMap<Uuid, broadcast::Sender<StreamChunk>>>,
}

impl Default for RcaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl RcaRegistry {
    pub fn new() -> Self {
        Self {
            active: Mutex::new(HashMap::new()),
        }
    }

    pub async fn subscribe(&self, issue_id: Uuid) -> Option<broadcast::Receiver<StreamChunk>> {
        self.active.lock().await.get(&issue_id).map(|tx| tx.subscribe())
    }

    pub async fn is_running(&self, issue_id: Uuid) -> bool {
        self.active.lock().await.contains_key(&issue_id)
    }

    async fn register(&self, issue_id: Uuid) -> (broadcast::Sender<StreamChunk>, broadcast::Receiver<StreamChunk>) {
        let (tx, rx) = broadcast::channel(4096);
        self.active.lock().await.insert(issue_id, tx.clone());
        (tx, rx)
    }

    async fn remove(&self, issue_id: Uuid) {
        self.active.lock().await.remove(&issue_id);
    }
}

fn tool_label(tool_name: &str, input: &str) -> String {
    let base = match tool_name {
        n if n.contains("discover_data_sources") => "发现数据源",
        n if n.contains("check_service_health") => "检查服务健康",
        n if n.contains("search_logs") => "搜索日志",
        n if n.contains("search_traces") => "搜索链路追踪",
        n if n.contains("query_metrics") => "查询指标 (PromQL)",
        n if n.contains("fetch_source_code") => "获取源代码",
        _ => "调查中",
    };
    let detail = serde_json::from_str::<serde_json::Value>(input).ok().and_then(|v| {
        v.get("service")
            .and_then(|s| s.as_str())
            .map(String::from)
            .or_else(|| v.get("file_path").and_then(|s| s.as_str()).map(String::from))
            .or_else(|| v.get("label").and_then(|s| s.as_str()).map(String::from))
            .or_else(|| v.get("keywords").and_then(|s| s.as_str()).map(String::from))
    });
    match detail {
        Some(d) => format!("{} — {}", base, d),
        None => base.to_string(),
    }
}

/// Extract target service name from issue metadata, title, and description.
fn extract_target_service(issue: &Issue) -> Option<String> {
    if let Some(ref meta) = issue.rca_result {
        for key in &["service", "service_name"] {
            if let Some(s) = meta.get("labels").and_then(|l| l.get(*key)).and_then(|v| v.as_str())
                && !s.is_empty()
            {
                return Some(s.to_string());
            }
            if let Some(s) = meta.get(*key).and_then(|v| v.as_str())
                && !s.is_empty()
            {
                return Some(s.to_string());
            }
        }
        if let Some(labels) = meta.get("labels").and_then(|v| v.as_object()) {
            for key in &["container", "pod", "job", "app"] {
                if let Some(s) = labels.get(*key).and_then(|v| v.as_str())
                    && !s.is_empty()
                {
                    return Some(s.to_string());
                }
            }
        }
    }
    None
}

/// Build the system prompt that tells the AI how to investigate.
fn build_system_prompt(issue: &Issue, target_service: &Option<String>) -> String {
    let description = issue.description.as_deref().unwrap_or("N/A");
    let meta = issue
        .rca_result
        .as_ref()
        .map(|v| serde_json::to_string_pretty(v).unwrap_or_default())
        .unwrap_or_else(|| "N/A".to_string());

    let target_block = if let Some(svc) = target_service {
        format!(
            r#"
## 🎯 调查目标（已从告警元数据中提取）
**目标服务: `{svc}`**
你的全部调查必须围绕这个服务展开。不要调查其他不相关的服务。"#
        )
    } else {
        String::new()
    };

    let discover_instruction = if target_service.is_some() {
        "- `discover_data_sources` — 发现监控系统中有哪些 label、service、namespace、metric。仅在目标服务名称不确定时使用"
    } else {
        "- `discover_data_sources` — 发现监控系统中有哪些 label、service、namespace、metric。**必须第一步调用**来确认目标服务"
    };

    let first_step = if let Some(svc) = target_service {
        format!(
            r#"**第一步：直接检查目标服务**
目标服务已确定为 `{svc}`，直接用 `check_service_health` 检查它的健康状态。
不需要先调 discover_data_sources（除非 check_service_health 返回的数据为空）。"#
        )
    } else {
        r#"**第一步：锁定目标服务**
从问题的标题、描述、告警元数据中提取相关的 service name / namespace。
用 `discover_data_sources` 确认该服务在监控系统中的确切名称。"#
            .to_string()
    };

    format!(
        r#"你是 Ops RCA (Root Cause Analysis) 分析师。你需要像一个资深 SRE 一样，逐步调查以下问题的根本原因。

## 待调查问题
- **标题**: {title}
- **严重性**: {severity}
- **来源**: {source}
- **类型**: {issue_type}
- **描述**: {description}
- **告警元数据**:
```json
{meta}
```
- **创建时间**: {created_at}
{target_block}

## 工具

{discover_instruction}
- `check_service_health` — 一次性获取服务的错误率、延迟、CPU/内存/重启等健康概览
- `search_logs` — 按 service/namespace/关键词/level 搜索日志（不需要写 LogQL）
- `search_traces` — 搜索分布式追踪（按 service/status/最小延迟过滤）
- `query_metrics` — 自定义 PromQL 查询（仅在 check_service_health 不够时使用）
- `fetch_source_code` — 从 GitHub 获取源代码

## 调查规则（严格遵守）

**核心规则：每次只调一个工具。分析结果后，再决定下一步。**

**绝对禁止：**
- ❌ 一次性调多个工具
- ❌ 不看结果就决定下一步
- ❌ **调查不相关的服务** — 只查目标服务和直接上下游依赖，最多查 2 个服务
- ❌ **绝对禁止向用户提问或等待输入** — 你是全自动 agent，没有人在另一端
- ❌ 说"请告诉我"、"你想查什么"、"请提供"之类的话
- ❌ 在调查中途停下来输出总结 — 必须持续调用工具直到完成
- ❌ 对 discover_data_sources 返回的所有服务逐个检查
- ❌ 用完全相同的参数重复调用同一个工具

**空结果处理（关键！）：**
- ⚠️ 如果告警标题明确包含错误信息（如 ZeroDivisionError），但 search_logs 返回空，**必须换策略重试**：
  1. 先用 `keywords` 参数（从告警标题/描述提取关键词），不设 `level` 过滤
  2. 如果还是空，去掉所有过滤器搜原始日志，确认 Loki 中是否有该服务的数据
  3. 用 `discover_data_sources` 查 `service_name` label 的实际值，确认服务名拼写是否正确
- ⚠️ 同一工具用**不同参数**调用不算重复调用 — 这是正常的调查策略切换
- ⚠️ **绝不允许**告警说有错误但你结论说"服务健康" — 如果查不到数据，必须在报告中说明数据缺失，而不是得出与告警矛盾的结论

你是一个完全自主的 agent。在整个调查过程中，你不能停下来等待人类输入。
每次分析完一个工具的结果后，必须立即决定并调用下一个工具。
如果某个数据源不可用（如 HTTP 404），跳过它，用其他可用数据源继续调查。
如果所有遥测数据不足，仍然要基于告警元数据给出最佳分析和建议。
调查完成的标志是你输出了完整的报告（包含 Hypotheses、Root Cause、Mitigation 等所有章节）。

### 调查流程

{first_step}

**第二步：聚焦调查（只查目标服务）**
从问题标题和描述中提取关键错误信息（如异常类名、HTTP 状态码、错误消息），用 `search_logs(keywords="提取的关键词")` 精确搜索。
然后用 `search_traces` 查找失败或慢请求。
**绝对禁止**扫描所有 namespace 或逐个检查 discover 返回的服务列表。

**第三步：深入排查**
根据发现灵活深入：
- 错误率高 → `search_logs`（搜 error/exception）
- 延迟异常 → `search_traces`（找慢请求链路）
- 重启频繁 → `search_logs`（搜 OOMKilled/CrashLoopBackOff）
- 日志有文件行号 → `fetch_source_code`
- **只有当**目标服务自身完全正常时，才查询一个最可能的上游依赖服务

每次分析完一个工具结果后，直接调用下一个工具。不要输出总结或解释你的计划。

## 输出格式

在所有调查步骤完成后，用以下**精确格式**输出最终 RCA 报告。标题必须完全匹配（系统会根据标题解析结构）：

### Hypotheses
用编号列表列出你的假设（1-3个），每个假设包含：
1. **假设标题**：一句话概括
   - 支持证据
   - 不确定因素

### Key Findings
用编号列表列出关键发现（2-5个），每个发现：
1. **发现标题**：一句话概括具体技术发现
   - 具体数据/证据
   - Observations: 相关观察点

### Root Cause
> 用 blockquote 包裹 1-2 句话的核心根因结论（这会被高亮展示给用户）

详细解释根因，包含具体的技术细节、时间线和影响。

### Impact
- 受影响的服务/组件
- 用户影响程度和范围
- 持续时间

### Immediate Mitigation
用编号步骤列出立即可执行的修复方案，每步要具体到命令或代码级别：
1. **步骤标题**
   描述
   ```语言
   具体命令或代码
   ```

### Long-term Improvements
用编号列出长期改进方案：
1. **改进标题**：具体描述

注意：每次工具调用都会实时展示给用户，所以你的分析过程就是报告的一部分。"#,
        title = issue.title,
        severity = issue.severity,
        source = issue.source,
        issue_type = issue.issue_type,
        created_at = issue.created_at,
    )
}

/// Generate a short-lived service JWT for RCA agent → MCP callbacks.
fn generate_rca_token(jwt_secret: &str, tenant_id: Option<Uuid>) -> Option<String> {
    let now = chrono::Utc::now().timestamp() as usize;
    let claims = crate::middleware::auth::Claims {
        sub: Uuid::nil(),
        role: "tenant_admin".to_string(),
        tenant_id,
        username: "rca-agent".to_string(),
        token_type: "access".to_string(),
        iat: now,
        exp: now + 7200,
    };
    jsonwebtoken::encode(
        &jsonwebtoken::Header::default(),
        &claims,
        &jsonwebtoken::EncodingKey::from_secret(jwt_secret.as_bytes()),
    )
    .ok()
}

/// Write MCP config for the opsk-rca server to a temp file.
async fn write_mcp_config(
    work_dir: &std::path::Path,
    config: &AppConfig,
    tenant_id: Option<Uuid>,
    issue_time: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    let base_url =
        std::env::var("SELF_BASE_URL").unwrap_or_else(|_| format!("http://localhost:{}", config.backend_port));
    let rca_url = format!(
        "{}/api/mcp/rca?issue_time={}",
        base_url.trim_end_matches('/'),
        urlencoding::encode(&issue_time.to_rfc3339())
    );

    let token = generate_rca_token(&config.jwt_secret, tenant_id);
    let mut server_config = serde_json::json!({
        "type": "http",
        "url": rca_url
    });
    let mut headers = serde_json::Map::new();
    if let Some(t) = token {
        headers.insert("Authorization".to_string(), serde_json::json!(format!("Bearer {}", t)));
    }
    headers.insert("X-Issue-Time".to_string(), serde_json::json!(issue_time.to_rfc3339()));
    server_config
        .as_object_mut()
        .unwrap()
        .insert("headers".to_string(), serde_json::Value::Object(headers));

    let mcp_config = serde_json::json!({
        "mcpServers": {
            "opsk-rca": server_config
        }
    });

    let config_path = work_dir.join(".mcp-rca.json");
    if let Err(e) = tokio::fs::create_dir_all(work_dir).await {
        tracing::error!("Failed to create work dir: {e}");
        return None;
    }
    match tokio::fs::write(
        &config_path,
        serde_json::to_string_pretty(&mcp_config).unwrap_or_default(),
    )
    .await
    {
        Ok(()) => Some(config_path.to_string_lossy().to_string()),
        Err(e) => {
            tracing::error!("Failed to write MCP config: {e}");
            None
        }
    }
}

/// Extract error keywords from issue title/description using common patterns.
fn extract_error_keywords(title: &str, description: Option<&str>) -> Vec<String> {
    let re = Regex::new(r"(?i)([A-Z][a-zA-Z]*(?:Error|Exception|Fault)|(?:\b[45]\d{2}\b)|OOM(?:Killed)?|timeout|crash|panic|segfault|SIGKILL|SIGTERM)")
        .unwrap();
    let mut keywords = Vec::new();
    let mut seen = HashSet::new();

    for text in std::iter::once(title).chain(description.into_iter()) {
        for cap in re.captures_iter(text) {
            let kw = cap[1].to_string();
            if seen.insert(kw.to_lowercase()) {
                keywords.push(kw);
            }
        }
    }
    keywords
}

/// Write issue context and Claude Code hooks to the RCA work directory.
///
/// Hooks enforce hard constraints that prompts alone can't guarantee:
/// - PostToolUse on search_logs: blocks empty results when alert clearly mentions errors,
///   forcing the agent to retry with extracted keywords instead of concluding "healthy".
/// - PostToolUse on search_traces: same logic for empty trace results.
/// - Stop: prevents the agent from finishing if it never found log evidence for an error-bearing alert.
async fn write_rca_hooks(work_dir: &std::path::Path, issue: &Issue) {
    let claude_dir = work_dir.join(".claude");
    let hooks_dir = claude_dir.join("hooks");

    if let Err(e) = tokio::fs::create_dir_all(&hooks_dir).await {
        tracing::warn!("Failed to create .claude/hooks dir for RCA: {}", e);
        return;
    }

    // ── Extract error keywords from issue ──────────────────────────
    let keywords = extract_error_keywords(&issue.title, issue.description.as_deref());
    let has_error_keywords = !keywords.is_empty();

    // ── Write issue context for hooks to read ──────────────────────
    let context = serde_json::json!({
        "title": issue.title,
        "description": issue.description,
        "severity": issue.severity,
        "source": issue.source,
        "error_keywords_in_title": has_error_keywords,
        "extracted_keywords": keywords,
    });
    let ctx_path = claude_dir.join("issue-context.json");
    if let Err(e) = tokio::fs::write(&ctx_path, serde_json::to_string_pretty(&context).unwrap_or_default()).await {
        tracing::warn!("Failed to write issue-context.json: {}", e);
        return;
    }

    // ── Write initial RCA state ────────────────────────────────────
    let state = serde_json::json!({
        "error_keywords_in_title": has_error_keywords,
        "extracted_keywords": keywords,
        "logs_found": false,
        "traces_found": false,
        "logs_retry_count": 0,
        "stop_blocked_count": 0,
    });
    let state_path = claude_dir.join("rca-state.json");
    if let Err(e) = tokio::fs::write(&state_path, serde_json::to_string_pretty(&state).unwrap_or_default()).await {
        tracing::warn!("Failed to write rca-state.json: {}", e);
        return;
    }

    // ── Hook: PostToolUse on search_logs ───────────────────────────
    let post_search_logs = r#"#!/usr/bin/env bash
# PostToolUse hook: force retry with keywords when logs are empty but alert has errors.
# Exit 0 = approve, Exit 2 = block (stdout shown to agent as reason).
set -euo pipefail
INPUT=$(cat)
OUTPUT=$(echo "$INPUT" | jq -r '.tool_output // ""')

# Non-empty result → approve and mark logs as found
if ! echo "$OUTPUT" | grep -q "^(No "; then
  STATE=".claude/rca-state.json"
  if [ -f "$STATE" ]; then
    TMP=$(mktemp)
    jq '.logs_found = true' "$STATE" > "$TMP" && mv "$TMP" "$STATE"
  fi
  exit 0
fi

# Empty result — check if alert has error keywords
CTX=".claude/issue-context.json"
[ ! -f "$CTX" ] && exit 0

HAS_ERRORS=$(jq -r '.error_keywords_in_title // false' "$CTX")
[ "$HAS_ERRORS" != "true" ] && exit 0

# Check retry count — only block twice max to prevent infinite loops
STATE=".claude/rca-state.json"
if [ -f "$STATE" ]; then
  RETRY=$(jq -r '.logs_retry_count // 0' "$STATE")
  if [ "$RETRY" -ge 2 ]; then
    exit 0
  fi
fi

# Check if agent already used keywords parameter
TOOL_INPUT=$(echo "$INPUT" | jq -r '.tool_input // "{}"')
HAS_KW=$(echo "$TOOL_INPUT" | jq -r '.keywords // empty')
if [ -n "$HAS_KW" ]; then
  # Already used keywords, still empty — real data gap, allow
  exit 0
fi

# Block: force retry with keywords extracted from alert
KEYWORDS=$(jq -r '.extracted_keywords // [] | join(",")' "$CTX")
if [ -f "$STATE" ]; then
  TMP=$(mktemp)
  jq '.logs_retry_count += 1' "$STATE" > "$TMP" && mv "$TMP" "$STATE"
fi

cat <<MSG
日志搜索返回空结果，但告警明确提到错误关键词。
请用 keywords 参数重试: search_logs(service=同上, keywords="${KEYWORDS}")
不要设 level 过滤器，直接用关键词搜索原始日志文本。
MSG
exit 2
"#;

    // ── Hook: PostToolUse on search_traces ─────────────────────────
    let post_search_traces = r#"#!/usr/bin/env bash
# PostToolUse hook: suggest broader trace search when results are empty.
set -euo pipefail
INPUT=$(cat)
OUTPUT=$(echo "$INPUT" | jq -r '.tool_output // ""')

# Non-empty result → approve and mark traces as found
if ! echo "$OUTPUT" | grep -q "^(No "; then
  STATE=".claude/rca-state.json"
  if [ -f "$STATE" ]; then
    TMP=$(mktemp)
    jq '.traces_found = true' "$STATE" > "$TMP" && mv "$TMP" "$STATE"
  fi
  exit 0
fi

# Empty result — check if agent used status=error (default)
CTX=".claude/issue-context.json"
[ ! -f "$CTX" ] && exit 0

HAS_ERRORS=$(jq -r '.error_keywords_in_title // false' "$CTX")
[ "$HAS_ERRORS" != "true" ] && exit 0

TOOL_INPUT=$(echo "$INPUT" | jq -r '.tool_input // "{}"')
STATUS=$(echo "$TOOL_INPUT" | jq -r '.status // "error"')

# If searched with status=error and got nothing, suggest broader search
if [ "$STATUS" = "error" ]; then
  cat <<MSG
链路追踪搜索返回空结果。建议：
1. 去掉 status 过滤，搜索全部 trace: search_traces(service=同上, status="")
2. 或者用 min_duration_ms 搜索慢请求: search_traces(service=同上, min_duration_ms=1000, status="")
MSG
  exit 2
fi

# Already tried broader search — allow
exit 0
"#;

    // ── Hook: Stop — validate investigation completeness ──────────
    let validate_stop = r#"#!/usr/bin/env bash
# Stop hook: prevent agent from concluding before finding evidence.
set -euo pipefail

STATE=".claude/rca-state.json"
[ ! -f "$STATE" ] && exit 0

HAS_ERRORS=$(jq -r '.error_keywords_in_title // false' "$STATE")
[ "$HAS_ERRORS" != "true" ] && exit 0

LOGS_FOUND=$(jq -r '.logs_found // false' "$STATE")
STOP_COUNT=$(jq -r '.stop_blocked_count // 0' "$STATE")

# Only block once to prevent infinite loop
[ "$STOP_COUNT" -ge 1 ] && exit 0

if [ "$LOGS_FOUND" != "true" ]; then
  TMP=$(mktemp)
  jq '.stop_blocked_count += 1' "$STATE" > "$TMP" && mv "$TMP" "$STATE"
  KEYWORDS=$(jq -r '.extracted_keywords // [] | join(",")' ".claude/issue-context.json" 2>/dev/null)
  cat <<MSG
调查不完整：告警包含错误关键词 (${KEYWORDS}) 但你没有找到相关日志证据。
请尝试：
1. search_logs(service=目标服务, keywords="${KEYWORDS}") 不设 level 过滤器
2. 如果 service_name 匹配不到，用 discover_data_sources(label="service_name") 确认实际名称
3. 如果确实无数据，在报告中明确说明"遥测数据缺失，无法确认根因"而非"服务正常"
MSG
  exit 2
fi

exit 0
"#;

    // ── Write hook scripts ─────────────────────────────────────────
    let scripts: &[(&str, &str)] = &[
        ("post-search-logs.sh", post_search_logs),
        ("post-search-traces.sh", post_search_traces),
        ("validate-stop.sh", validate_stop),
    ];

    for (name, content) in scripts {
        let path = hooks_dir.join(name);
        if let Err(e) = tokio::fs::write(&path, content).await {
            tracing::warn!("Failed to write hook {}: {}", name, e);
            return;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).await;
        }
    }

    // ── Write settings.local.json with hooks config ────────────────
    let logs_hook_path = hooks_dir.join("post-search-logs.sh").to_string_lossy().to_string();
    let traces_hook_path = hooks_dir.join("post-search-traces.sh").to_string_lossy().to_string();
    let stop_hook_path = hooks_dir.join("validate-stop.sh").to_string_lossy().to_string();

    let settings = serde_json::json!({
        "hooks": {
            "PostToolUse": [
                {
                    "matcher": "mcp__opsk-rca__search_logs",
                    "hooks": [{"type": "command", "command": logs_hook_path}]
                },
                {
                    "matcher": "mcp__opsk-rca__search_traces",
                    "hooks": [{"type": "command", "command": traces_hook_path}]
                }
            ],
            "Stop": [
                {
                    "hooks": [{"type": "command", "command": stop_hook_path}]
                }
            ]
        }
    });

    let settings_path = claude_dir.join("settings.local.json");
    match serde_json::to_string_pretty(&settings) {
        Ok(content) => {
            if let Err(e) = tokio::fs::write(&settings_path, content).await {
                tracing::warn!("Failed to write RCA hooks settings: {}", e);
            } else {
                tracing::info!("Wrote RCA hooks to {:?} (keywords: {:?})", settings_path, keywords);
            }
        }
        Err(e) => tracing::warn!("Failed to serialize RCA hook settings: {}", e),
    }
}

/// Execute agent-driven RCA analysis for an issue.
///
/// Instead of pre-fetching all telemetry, this gives Claude CLI MCP tools
/// and lets the AI drive the investigation iteratively. Each tool call is
/// streamed to the frontend as a step card with data and analysis.
pub async fn run_rca(
    pool: PgPool,
    config: Arc<AppConfig>,
    registry: Arc<RcaRegistry>,
    issue: Issue,
    notification_tx: Option<broadcast::Sender<crate::models::notification::Notification>>,
) {
    let issue_id = issue.id;
    let issue_title = issue.title.clone();

    let _ = sqlx::query(
        "UPDATE issues SET rca_started_at = NOW(), status = 'investigating', updated_at = NOW() WHERE id = $1",
    )
    .bind(issue_id)
    .execute(&pool)
    .await;

    let (tx, _rx) = registry.register(issue_id).await;

    // ─── Build MCP config for RCA tools ─────────────────────
    let work_dir = PathBuf::from(&config.claude_work_dir)
        .join("rca")
        .join(issue_id.to_string());
    let mcp_config_path = write_mcp_config(&work_dir, &config, issue.tenant_id, issue.created_at).await;

    // ─── Write hooks for investigation quality gates ────────
    write_rca_hooks(&work_dir, &issue).await;

    // ─── Build system prompt and run agent ───────────────────
    let target_service = extract_target_service(&issue);
    let system_prompt = build_system_prompt(&issue, &target_service);

    let initial_message = if let Some(svc) = target_service {
        format!(
            "开始自主调查。你是全自动 RCA agent，不会有人回复你。\
             目标服务是 `{svc}`。第一步：直接调用 check_service_health 检查 `{svc}` 的健康状态。"
        )
    } else {
        "开始自主调查。你是全自动 RCA agent，不会有人回复你。每一步分析完后必须立即调用下一个工具。第一步：调用 discover_data_sources。".to_string()
    };

    let provider = super::claude::load_provider_env_from_db(&pool).await;
    let model = provider.model.unwrap_or_else(|| config.claude_model.clone());

    let agent = crate::services::agent::claude::ClaudeAgent {
        bin_path: config.claude_bin.clone(),
        work_dir,
        timeout: Duration::from_millis(config.claude_timeout_ms),
    };

    let agent_config = AgentSessionConfig {
        session_id: None,
        message: initial_message,
        system_prompt: Some(system_prompt),
        model,
        max_turns: 100,
        permission_mode: super::claude::AgentPermission::Bypass.cli_flag().to_string(),
        allowed_tools: Vec::new(),
        disallowed_tools: Vec::new(),
        env_vars: provider.env_vars,
        mcp_config_path,
        images: Vec::new(),
    };

    let analysis_start = Instant::now();
    let mut full_text = String::new();
    let mut step_counter = 0u32;
    let mut current_step_id: Option<String> = None;
    let mut current_tool_name = String::new();
    let mut pending_reasoning = String::new();
    let mut step_start_time = Instant::now();
    let mut seen_tool_results: HashSet<String> = HashSet::new();
    let mut saved_steps: Vec<serde_json::Value> = Vec::new();
    let mut current_step_record: Option<StepRecord> = None;
    let mut got_done = false;

    // Send initial step
    let _ = tx.send(StreamChunk::Step {
        step: "init".to_string(),
        status: "running".to_string(),
        label: "AI 开始调查...".to_string(),
        summary: None,
        duration_ms: None,
    });

    match agent.run(agent_config) {
        Ok(mut rx) => {
            while let Some(event) = rx.recv().await {
                match &event {
                    AgentEvent::Text { content } => {
                        if current_step_id.is_some() {
                            let step_id = current_step_id.clone().unwrap();
                            let _ = tx.send(StreamChunk::StepAnalysis {
                                step_id,
                                content: content.clone(),
                            });
                            if let Some(ref mut rec) = current_step_record {
                                rec.analysis.push_str(content);
                            }
                        } else {
                            pending_reasoning.push_str(content);
                        }

                        if full_text.len() < 100_000 {
                            let remaining = 100_000 - full_text.len();
                            if content.len() <= remaining {
                                full_text.push_str(content);
                            } else {
                                full_text.push_str(&content[..remaining]);
                                full_text.push_str("\n\n[OUTPUT TRUNCATED]");
                            }
                        }
                    }
                    AgentEvent::ToolUse {
                        tool_name,
                        content: input,
                    } => {
                        if let Some(prev_id) = current_step_id.take() {
                            let prev_empty = current_step_record
                                .as_ref()
                                .map(|r| r.data_text.is_empty() && r.analysis.is_empty())
                                .unwrap_or(false);
                            if prev_empty {
                                // Cancel empty card — no data ever arrived
                                let _ = tx.send(StreamChunk::StepComplete {
                                    step_id: prev_id,
                                    summary: "(skipped)".to_string(),
                                    duration_ms: 0,
                                });
                                current_step_record.take();
                                step_counter -= 1;
                            } else {
                                let elapsed = step_start_time.elapsed().as_millis() as u64;
                                let _ = tx.send(StreamChunk::StepComplete {
                                    step_id: prev_id,
                                    summary: String::new(),
                                    duration_ms: elapsed,
                                });
                                if let Some(rec) = current_step_record.take() {
                                    saved_steps.push(rec.into_json(elapsed));
                                }
                            }
                        }

                        step_counter += 1;
                        let step_id = format!("step_{}", step_counter);
                        current_step_id = Some(step_id.clone());
                        current_tool_name.clone_from(tool_name);
                        step_start_time = Instant::now();

                        let reasoning = std::mem::take(&mut pending_reasoning);
                        let reasoning_trimmed = reasoning.trim().to_string();
                        let label = tool_label(tool_name, input);

                        current_step_record = Some(StepRecord {
                            step_id: step_id.clone(),
                            tool_name: tool_name.clone(),
                            reasoning: reasoning_trimmed.clone(),
                            label: label.clone(),
                            analysis: String::new(),
                            data_text: String::new(),
                        });

                        let _ = tx.send(StreamChunk::StepStart {
                            step_id,
                            tool_name: tool_name.clone(),
                            reasoning: reasoning_trimmed,
                            label,
                        });
                    }
                    AgentEvent::ToolResult {
                        tool_name,
                        content: result_text,
                    } => {
                        // Deduplicate: tool results can arrive from both assistant
                        // and user events in Claude CLI stream-json output.
                        let dedup_key = format!("{}:{}", tool_name, &result_text[..result_text.len().min(200)]);
                        if seen_tool_results.contains(&dedup_key) {
                            continue;
                        }
                        seen_tool_results.insert(dedup_key);

                        if current_step_id.is_none() {
                            step_counter += 1;
                            let step_id = format!("step_{}", step_counter);
                            current_step_id = Some(step_id.clone());
                            current_tool_name.clone_from(tool_name);
                            step_start_time = Instant::now();
                            let label = tool_label(tool_name, "");
                            current_step_record = Some(StepRecord {
                                step_id: step_id.clone(),
                                tool_name: tool_name.clone(),
                                reasoning: String::new(),
                                label: label.clone(),
                                analysis: String::new(),
                                data_text: String::new(),
                            });
                            let _ = tx.send(StreamChunk::StepStart {
                                step_id,
                                tool_name: tool_name.clone(),
                                reasoning: String::new(),
                                label,
                            });
                        }

                        if let Some(ref step_id) = current_step_id {
                            let truncated = if result_text.len() > 5000 {
                                format!(
                                    "{}...\n[truncated, {} chars total]",
                                    &result_text[..5000],
                                    result_text.len()
                                )
                            } else {
                                result_text.clone()
                            };
                            if let Some(ref mut rec) = current_step_record {
                                rec.data_text = truncated.clone();
                            }
                            let _ = tx.send(StreamChunk::StepData {
                                step_id: step_id.clone(),
                                tool_name: tool_name.clone(),
                                data_text: truncated,
                            });
                        }
                    }
                    AgentEvent::Done {
                        content,
                        session_id,
                        duration_ms,
                    } => {
                        if !content.is_empty() {
                            full_text = content.clone();
                        }
                        got_done = true;
                        let _ = tx.send(StreamChunk::Done {
                            content: content.clone(),
                            session_id: session_id.clone(),
                            duration_ms: *duration_ms,
                        });
                    }
                    AgentEvent::Error { message } => {
                        tracing::warn!("RCA agent error for issue {}: {}", issue_id, message);
                        let _ = tx.send(StreamChunk::Error {
                            message: message.clone(),
                        });
                    }
                    _ => {}
                }
            }

            // If we never got a Done event (timeout / max_turns exhausted), send accumulated text
            if !got_done {
                let _ = tx.send(StreamChunk::Done {
                    content: full_text.clone(),
                    session_id: None,
                    duration_ms: analysis_start.elapsed().as_millis() as u64,
                });
            }

            // Complete final step
            if let Some(last_id) = current_step_id.take() {
                let elapsed = step_start_time.elapsed().as_millis() as u64;
                let _ = tx.send(StreamChunk::StepComplete {
                    step_id: last_id,
                    summary: String::new(),
                    duration_ms: elapsed,
                });
                if let Some(rec) = current_step_record.take() {
                    saved_steps.push(rec.into_json(elapsed));
                }
            }

            let total_elapsed = analysis_start.elapsed().as_millis() as u64;
            let _ = tx.send(StreamChunk::Step {
                step: "complete".to_string(),
                status: "done".to_string(),
                label: "调查完成".to_string(),
                summary: Some(format!("{}s, {} 个调查步骤", total_elapsed / 1000, step_counter)),
                duration_ms: Some(total_elapsed),
            });

            // Persist result (analysis text + investigation steps)
            let rca_json = serde_json::json!({
                "analysis": full_text,
                "steps": saved_steps
            });
            let _ = sqlx::query(
                r#"UPDATE issues SET rca_result = $2, rca_completed_at = NOW(),
                   status = 'rca_done', updated_at = NOW() WHERE id = $1"#,
            )
            .bind(issue_id)
            .bind(&rca_json)
            .execute(&pool)
            .await;

            tracing::info!(
                "RCA completed for issue {} ({} chars, {} steps)",
                issue_id,
                full_text.len(),
                step_counter
            );

            if let Some(ref ntx) = notification_tx {
                let pool_n = pool.clone();
                let ntx_n = ntx.clone();
                let title_n = issue_title.clone();
                tokio::spawn(async move {
                    crate::services::notification::notify_tenant_admins(
                        &pool_n,
                        None,
                        "rca_completed",
                        &format!("RCA completed: {}", title_n),
                        &format!("Root cause analysis finished for \"{}\"", title_n),
                        serde_json::json!({ "status": "success" }),
                        Some(issue_id),
                        None,
                        Some(&ntx_n),
                    )
                    .await;
                });
            }
        }
        Err(e) => {
            let error_msg = format!("Failed to spawn Claude CLI for RCA: {}", e);
            tracing::error!("{}", error_msg);

            let _ = tx.send(StreamChunk::Step {
                step: "analysis".to_string(),
                status: "error".to_string(),
                label: "分析失败".to_string(),
                summary: Some(error_msg.clone()),
                duration_ms: None,
            });
            let _ = tx.send(StreamChunk::Error {
                message: error_msg.clone(),
            });

            let rca_json = serde_json::json!({ "error": error_msg });
            let _ = sqlx::query(
                r#"UPDATE issues SET rca_result = $2, rca_completed_at = NOW(),
                   status = 'open', updated_at = NOW() WHERE id = $1"#,
            )
            .bind(issue_id)
            .bind(&rca_json)
            .execute(&pool)
            .await;

            if let Some(ref ntx) = notification_tx {
                let pool_n = pool.clone();
                let ntx_n = ntx.clone();
                let title_n = issue_title.clone();
                tokio::spawn(async move {
                    crate::services::notification::notify_tenant_admins(
                        &pool_n,
                        None,
                        "rca_failed",
                        &format!("RCA failed: {}", title_n),
                        &format!("Root cause analysis failed for \"{}\"", title_n),
                        serde_json::json!({ "status": "error" }),
                        Some(issue_id),
                        None,
                        Some(&ntx_n),
                    )
                    .await;
                });
            }
        }
    }

    registry.remove(issue_id).await;
}
