use serde::Deserialize;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc;

use super::{Agent, AgentEvent, AgentSessionConfig, ImageData};

/// Claude Code CLI agent implementation.
pub struct ClaudeAgent {
    pub bin_path: String,
    pub work_dir: PathBuf,
    pub timeout: Duration,
}

impl Agent for ClaudeAgent {
    fn name(&self) -> &str {
        "claude-code"
    }

    fn run(&self, config: AgentSessionConfig) -> Result<mpsc::Receiver<AgentEvent>, anyhow::Error> {
        let (tx, rx) = mpsc::channel(64);

        let has_images = !config.images.is_empty();

        // Build CLI args
        let args = build_args(
            &config.message,
            config.session_id.as_deref(),
            config.system_prompt.as_deref(),
            has_images,
            &config.permission_mode,
            &config.disallowed_tools,
            &config.allowed_tools,
            config.mcp_config_path.as_deref(),
            &config.model,
            config.max_turns,
        );

        let timeout = self.timeout;

        tracing::info!(
            "ClaudeAgent: spawning model={}, {} images, args={:?} in {:?}",
            config.model,
            config.images.len(),
            &args,
            self.work_dir
        );

        // Build stdin payload for multimodal
        let stdin_data = if has_images {
            Some(build_stream_input(&config.message, &config.images))
        } else {
            None
        };

        let stdin_mode = if has_images {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        };

        let mut cmd = Command::new(&self.bin_path);
        cmd.args(&args)
            .current_dir(&self.work_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .stdin(stdin_mode)
            .kill_on_drop(true);

        // Remove inherited env vars that interfere
        cmd.env_remove("AWS_BEARER_TOKEN_BEDROCK");
        cmd.env_remove("AWS_BEARER_TOKEN");
        cmd.env_remove("CLAUDE_CODE_USE_BEDROCK");
        cmd.env_remove("ANTHROPIC_BASE_URL");
        cmd.env_remove("ANTHROPIC_API_KEY");

        // Set HOME to the work_dir so Claude CLI stores sessions on the PVC,
        // surviving pod restarts.
        cmd.env("HOME", &self.work_dir);

        for (key, value) in &config.env_vars {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| anyhow::anyhow!("Failed to spawn claude: {}", e))?;
        let child_stdin = child.stdin.take();
        let stdout = child.stdout.take().ok_or_else(|| anyhow::anyhow!("No stdout"))?;
        let stderr = child.stderr.take();

        let is_resume = config.session_id.is_some();
        let resume_session_id = config.session_id.clone();

        // Spawn the reader task -- it pushes events into the channel
        tokio::spawn(async move {
            // Write stdin first (for images)
            if let (Some(data), Some(mut stdin)) = (stdin_data, child_stdin) {
                tracing::info!("Writing {} bytes to claude stdin", data.len());
                if let Err(e) = stdin.write_all(data.as_bytes()).await {
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: format!("Failed to send images: {}", e),
                        })
                        .await;
                    let _ = child.kill().await;
                    return;
                }
                let _ = stdin.write_all(b"\n").await;
                drop(stdin);
            }

            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            let start = std::time::Instant::now();

            if is_resume {
                // ── Resume session: text output format ──────────────────────────
                // Read entire response as plain text (no JSON parsing, no replay).
                // stream-json requires --verbose which replays history; text format
                // gives us the clean response without that overhead.
                let mut full_text = String::new();
                loop {
                    if start.elapsed() > timeout {
                        let _ = tx
                            .send(AgentEvent::Error {
                                message: "Claude CLI timeout".to_string(),
                            })
                            .await;
                        let _ = child.kill().await;
                        break;
                    }

                    match tokio::time::timeout(Duration::from_secs(60), lines.next_line()).await {
                        Ok(Ok(Some(line))) => {
                            if !full_text.is_empty() {
                                full_text.push('\n');
                            }
                            full_text.push_str(&line);
                        }
                        Ok(Ok(None)) => break,
                        Ok(Err(e)) => {
                            let _ = tx
                                .send(AgentEvent::Error {
                                    message: format!("Read error: {}", e),
                                })
                                .await;
                            break;
                        }
                        Err(_) => continue,
                    }
                }

                let trimmed = full_text.trim().to_string();

                // Detect lost session: CLI may print the error to stdout in text mode
                if is_session_not_found(&trimmed) {
                    tracing::warn!("Session file lost (stdout) — signalling client to start fresh");
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: SESSION_EXPIRED.to_string(),
                        })
                        .await;
                } else {
                    if !trimmed.is_empty() {
                        let _ = tx
                            .send(AgentEvent::Text {
                                content: trimmed.clone(),
                            })
                            .await;
                    }
                    let _ = tx
                        .send(AgentEvent::Done {
                            content: trimmed,
                            session_id: resume_session_id,
                            duration_ms: start.elapsed().as_millis() as u64,
                        })
                        .await;
                }
            } else {
                // ── New session: stream-json output format ──────────────────────
                loop {
                    if start.elapsed() > timeout {
                        let _ = tx
                            .send(AgentEvent::Error {
                                message: "Claude CLI timeout".to_string(),
                            })
                            .await;
                        let _ = child.kill().await;
                        break;
                    }

                    match tokio::time::timeout(Duration::from_secs(60), lines.next_line()).await {
                        Ok(Ok(Some(line))) => {
                            if line.trim().is_empty() {
                                continue;
                            }

                            // Debug logging
                            if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&line) {
                                let etype = raw.get("type").and_then(|v| v.as_str()).unwrap_or("?");
                                let subtype = raw.get("subtype").and_then(|v| v.as_str());
                                tracing::info!("Claude CLI event: type={} subtype={:?}", etype, subtype);
                                if etype == "user" {
                                    let preview = if line.len() > 500 { &line[..500] } else { &line };
                                    tracing::info!("Claude CLI user event: {}", preview);
                                }
                            }

                            let events = parse_stream_line(&line);
                            let mut is_done = false;
                            for event in events {
                                if matches!(&event, AgentEvent::Done { .. }) {
                                    is_done = true;
                                }
                                if tx.send(event).await.is_err() {
                                    // Consumer dropped
                                    let _ = child.kill().await;
                                    return;
                                }
                            }
                            if is_done {
                                break;
                            }
                        }
                        Ok(Ok(None)) => break,
                        Ok(Err(e)) => {
                            let _ = tx
                                .send(AgentEvent::Error {
                                    message: format!("Read error: {}", e),
                                })
                                .await;
                            break;
                        }
                        Err(_) => continue,
                    }
                }
            }

            // Read stderr and forward as error event (not just log)
            let mut stderr_content = String::new();
            if let Some(stderr) = stderr {
                let mut stderr_reader = BufReader::new(stderr);
                let _ = tokio::io::AsyncReadExt::read_to_string(&mut stderr_reader, &mut stderr_content).await;
                if !stderr_content.is_empty() {
                    tracing::warn!("Claude stderr: {}", &stderr_content[..stderr_content.len().min(2000)]);
                }
            }

            // Check exit code — non-zero means the CLI failed
            if let Ok(status) = child.wait().await
                && !status.success()
            {
                let detail: String = stderr_content.chars().take(500).collect();

                // Graceful fallback: if --resume failed because the session file
                // is gone (pod restart, cleanup, etc.), tell the frontend to start
                // a new session instead of showing a raw CLI error.
                if is_session_not_found(&detail) {
                    tracing::warn!("Session file lost — signalling client to start fresh");
                    let _ = tx
                        .send(AgentEvent::Error {
                            message: SESSION_EXPIRED.to_string(),
                        })
                        .await;
                } else {
                    let msg = if detail.is_empty() {
                        format!("Claude CLI exited with {}", status)
                    } else {
                        format!("Claude CLI error: {}", detail)
                    };
                    tracing::error!("{}", msg);
                    let _ = tx.send(AgentEvent::Error { message: msg }).await;
                }
            }
        });

        Ok(rx)
    }
}

// ─── Internal deserialization structs ────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ClaudeEvent {
    #[serde(rename = "type")]
    event_type: String,
    #[serde(default)]
    subtype: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    message: Option<ClaudeMessage>,
    #[serde(default)]
    result: Option<serde_json::Value>,
    #[serde(default)]
    duration_ms: Option<u64>,
    #[serde(default)]
    #[allow(dead_code)]
    tool_name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClaudeMessage {
    #[serde(default)]
    content: Vec<ClaudeContentBlock>,
}

#[derive(Debug, Deserialize)]
struct ClaudeContentBlock {
    #[serde(rename = "type")]
    block_type: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    input: Option<serde_json::Value>,
    /// MCP tool_result blocks use "content" instead of "text" for the result string
    #[serde(default)]
    content: Option<serde_json::Value>,
    /// tool_name from tool_reference blocks
    #[serde(default)]
    tool_name: Option<String>,
}

// ─── Helper functions ────────────────────────────────────────────────────────

/// Sentinel sent to the frontend when a --resume targets a missing session file.
const SESSION_EXPIRED: &str = "SESSION_EXPIRED";

fn is_session_not_found(output: &str) -> bool {
    output.contains("No conversation found with session ID")
}

/// Format tool input for display — extract the most relevant field instead of dumping raw JSON.
fn format_tool_input(tool_name: &str, input: &serde_json::Value) -> String {
    let fallback = || serde_json::to_string_pretty(input).unwrap_or_default();
    match tool_name {
        "Bash" => input
            .get("command")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(fallback),
        "Read" | "Write" | "Edit" => input
            .get("file_path")
            .and_then(|v| v.as_str())
            .map(String::from)
            .unwrap_or_else(fallback),
        "Grep" | "Glob" => input
            .get("pattern")
            .and_then(|v| v.as_str())
            .map(|p| {
                let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
                format!("{} in {}", p, path)
            })
            .unwrap_or_else(fallback),
        _ => fallback(),
    }
}

/// Build Claude CLI arguments.
/// When `use_stream_input` is true, uses `--input-format stream-json` and reads from stdin
/// instead of passing the message as a positional argument (needed for images).
#[allow(clippy::too_many_arguments)]
fn build_args(
    message: &str,
    session_id: Option<&str>,
    system_prompt: Option<&str>,
    use_stream_input: bool,
    permission_mode: &str,
    disallowed_tools: &[String],
    allowed_tools: &[String],
    mcp_config: Option<&str>,
    model: &str,
    max_turns: u32,
) -> Vec<String> {
    let mut args = vec!["-p".to_string()];

    if !use_stream_input {
        args.push(message.to_string());
    }

    // New sessions: stream-json + --verbose for real-time streaming with thinking.
    // Resume sessions: text format to avoid --verbose requirement (stream-json + -p
    // requires --verbose, but --verbose + --resume replays full conversation history).
    // Text format gives us the complete response without replay overhead.
    if session_id.is_none() {
        args.extend([
            "--output-format".to_string(),
            "stream-json".to_string(),
            "--verbose".to_string(),
        ]);
    } else {
        args.extend(["--output-format".to_string(), "text".to_string()]);
    }

    args.extend([
        "--max-turns".to_string(),
        max_turns.to_string(),
        "--model".to_string(),
        model.to_string(),
        "--permission-mode".to_string(),
        permission_mode.to_string(),
    ]);

    if !disallowed_tools.is_empty() {
        args.push("--disallowedTools".to_string());
        args.push(disallowed_tools.join(","));
    }

    if !allowed_tools.is_empty() {
        args.push("--allowedTools".to_string());
        args.push(allowed_tools.join(","));
    }

    if let Some(cfg) = mcp_config {
        args.push("--mcp-config".to_string());
        args.push(cfg.to_string());
    }

    if use_stream_input {
        args.push("--input-format".to_string());
        args.push("stream-json".to_string());
    }

    // System prompt only applies to new sessions; resume inherits the original.
    // Passing --system-prompt with --resume can cause the CLI to error out or
    // silently start a new session, leading to content replay / interrupted thinking.
    if session_id.is_none()
        && let Some(sp) = system_prompt
    {
        args.push("--system-prompt".to_string());
        args.push(sp.to_string());
    }

    if let Some(sid) = session_id {
        args.push("--resume".to_string());
        args.push(sid.to_string());
    }

    args
}

/// Build the stream-json input message containing text + optional images.
/// Format: {"type":"user","message":{"role":"user","content":[...]}}
fn build_stream_input(message: &str, images: &[ImageData]) -> String {
    let mut content = Vec::new();

    for img in images {
        content.push(serde_json::json!({
            "type": "image",
            "source": {
                "type": "base64",
                "media_type": img.media_type,
                "data": img.data,
            }
        }));
    }

    content.push(serde_json::json!({
        "type": "text",
        "text": message,
    }));

    let msg = serde_json::json!({
        "type": "user",
        "message": {
            "role": "user",
            "content": content,
        }
    });

    serde_json::to_string(&msg).unwrap_or_default()
}

/// Parse a single line of Claude stream-json output into AgentEvent(s)
pub fn parse_stream_line(line: &str) -> Vec<AgentEvent> {
    let mut events = Vec::new();

    let event: ClaudeEvent = match serde_json::from_str(line) {
        Ok(e) => e,
        Err(_) => return events,
    };

    tracing::debug!(
        "Claude CLI event: type={} subtype={:?}",
        event.event_type,
        event.subtype
    );

    match event.event_type.as_str() {
        "system" if event.subtype.as_deref() == Some("init") => {
            events.push(AgentEvent::Init {
                session_id: event.session_id,
            });
        }
        "assistant" => {
            if let Some(msg) = event.message {
                // Preserve original block order from Claude — only hoist thinking to front.
                // Text and tool_use blocks stay interleaved as Claude intended,
                // so "explain → execute → explain → execute" narrative flow is kept.
                let mut thinking_events = Vec::new();
                let mut ordered_events = Vec::new();

                for block in msg.content {
                    match block.block_type.as_str() {
                        "thinking" => {
                            if let Some(thinking) = block.thinking {
                                thinking_events.push(AgentEvent::Thinking { content: thinking });
                            }
                        }
                        "text" => {
                            if let Some(text) = block.text {
                                ordered_events.push(AgentEvent::Text { content: text });
                            }
                        }
                        "tool_use" => {
                            let name = block.name.unwrap_or_else(|| "unknown".to_string());
                            let input_str = block.input.map(|v| format_tool_input(&name, &v)).unwrap_or_default();
                            ordered_events.push(AgentEvent::ToolUse {
                                tool_name: name,
                                content: input_str,
                            });
                        }
                        "tool_result" => {
                            let content = block.text.unwrap_or_default();
                            let name = block.name.unwrap_or_else(|| "tool".to_string());
                            ordered_events.push(AgentEvent::ToolResult {
                                tool_name: name,
                                content,
                            });
                        }
                        _ => {}
                    }
                }

                events.extend(thinking_events);
                events.extend(ordered_events);
            }
        }
        "result" => {
            let content = event
                .result
                .map(|r| {
                    if let serde_json::Value::String(s) = r {
                        s
                    } else {
                        serde_json::to_string(&r).unwrap_or_default()
                    }
                })
                .unwrap_or_default();

            events.push(AgentEvent::Done {
                content,
                session_id: event.session_id,
                duration_ms: event.duration_ms.unwrap_or(0),
            });
        }
        "user" => {
            if let Some(msg) = event.message {
                for block in msg.content {
                    if block.block_type == "tool_result" {
                        let result_str = match &block.content {
                            Some(serde_json::Value::String(s)) => Some(s.clone()),
                            Some(serde_json::Value::Array(arr)) => {
                                let texts: Vec<String> = arr
                                    .iter()
                                    .filter_map(|item| item.get("text").and_then(|t| t.as_str()).map(|s| s.to_string()))
                                    .collect();
                                if texts.is_empty() { None } else { Some(texts.join("\n")) }
                            }
                            _ => block.text.clone(),
                        };
                        let name = block.tool_name.or(block.name).unwrap_or_else(|| "mcp_tool".to_string());
                        if let Some(content) = result_str
                            && !content.is_empty()
                        {
                            events.push(AgentEvent::ToolResult {
                                tool_name: name,
                                content,
                            });
                        }
                    }
                }
            }
        }
        other => {
            tracing::debug!("Unhandled Claude CLI event type: {}", other);
        }
    }

    events
}
