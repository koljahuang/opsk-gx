use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::error::{AppError, AppResult};
use crate::middleware::auth::AuthUser;
use crate::models::approval::Approval;
use crate::models::channel::Channel;
use crate::models::notification::Notification;
use crate::services::jira::JiraClient;

/// List approvals visible to the authenticated user.
/// - super_admin: see all
/// - tenant_admin (is_admin): see entire tenant's approvals (they are the approver)
/// - regular member: see only their own approvals
///
/// Optionally filter by status.
pub async fn list(pool: &PgPool, auth_user: &AuthUser, status: Option<&str>) -> AppResult<Vec<Approval>> {
    let approvals = if auth_user.is_super_admin() {
        match status {
            Some(s) => {
                sqlx::query_as::<_, Approval>(
                    "SELECT * FROM approvals WHERE status = $1 ORDER BY created_at DESC LIMIT 500",
                )
                .bind(s)
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, Approval>("SELECT * FROM approvals ORDER BY created_at DESC LIMIT 500")
                    .fetch_all(pool)
                    .await?
            }
        }
    } else if auth_user.is_admin() {
        // Tenant admin sees: own tenant's approvals + unassigned (tenant_id IS NULL)
        match status {
            Some(s) => {
                sqlx::query_as::<_, Approval>(
                    r#"SELECT * FROM approvals
                       WHERE (tenant_id IS NOT DISTINCT FROM $1 OR tenant_id IS NULL) AND status = $2
                       ORDER BY created_at DESC LIMIT 500"#,
                )
                .bind(auth_user.tenant_id)
                .bind(s)
                .fetch_all(pool)
                .await?
            }
            None => {
                sqlx::query_as::<_, Approval>(
                    "SELECT * FROM approvals WHERE (tenant_id IS NOT DISTINCT FROM $1 OR tenant_id IS NULL) ORDER BY created_at DESC LIMIT 500",
                )
                .bind(auth_user.tenant_id)
                .fetch_all(pool)
                .await?
            }
        }
    } else {
        // Regular users: see only their own approvals
        match status {
            Some(s) => sqlx::query_as::<_, Approval>(
                "SELECT * FROM approvals WHERE requested_by = $1 AND status = $2 ORDER BY created_at DESC LIMIT 500",
            )
            .bind(auth_user.user_id)
            .bind(s)
            .fetch_all(pool)
            .await?,
            None => {
                sqlx::query_as::<_, Approval>(
                    "SELECT * FROM approvals WHERE requested_by = $1 ORDER BY created_at DESC LIMIT 500",
                )
                .bind(auth_user.user_id)
                .fetch_all(pool)
                .await?
            }
        }
    };

    Ok(approvals)
}

/// Count pending approvals visible to the authenticated user.
pub async fn count_pending(pool: &PgPool, auth_user: &AuthUser) -> AppResult<i64> {
    let count = if auth_user.is_super_admin() {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM approvals WHERE status = 'pending'")
            .fetch_one(pool)
            .await?
    } else if auth_user.is_admin() {
        sqlx::query_scalar::<_, i64>(
            "SELECT COUNT(*) FROM approvals WHERE status = 'pending' AND (tenant_id IS NOT DISTINCT FROM $1 OR tenant_id IS NULL)",
        )
        .bind(auth_user.tenant_id)
        .fetch_one(pool)
        .await?
    } else {
        0
    };
    Ok(count)
}

/// Approve a pending approval request.
pub async fn approve(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    tx: &broadcast::Sender<Notification>,
) -> AppResult<Approval> {
    review_approval(pool, auth_user, id, "approved", tx).await
}

/// Reject a pending approval request.
pub async fn reject(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    tx: &broadcast::Sender<Notification>,
) -> AppResult<Approval> {
    review_approval(pool, auth_user, id, "rejected", tx).await
}

/// Shared logic for approve/reject: verify admin + tenant access, then update status.
async fn review_approval(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    new_status: &str,
    tx: &broadcast::Sender<Notification>,
) -> AppResult<Approval> {
    if !auth_user.is_admin() {
        return Err(AppError::Forbidden(format!(
            "Only admins can {} requests",
            new_status.trim_end_matches("ed")
        )));
    }

    // Load approval for validation
    let existing = sqlx::query_as::<_, Approval>("SELECT * FROM approvals WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Approval not found".to_string()))?;

    // Maker-checker: cannot approve/reject your own request
    if existing.requested_by == auth_user.user_id {
        return Err(AppError::Forbidden(
            "Cannot approve or reject your own request".to_string(),
        ));
    }

    // Tenant admin: verify the approval belongs to their tenant
    if !auth_user.is_super_admin() && existing.tenant_id.is_some() && existing.tenant_id != auth_user.tenant_id {
        return Err(AppError::Forbidden("Access denied".to_string()));
    }

    if existing.status != "pending" {
        return Err(AppError::BadRequest(format!("Approval is already {}", existing.status)));
    }

    let approval = sqlx::query_as::<_, Approval>(
        r#"UPDATE approvals SET
           status = $3,
           reviewed_by = $2,
           reviewed_at = NOW()
           WHERE id = $1 AND status = 'pending'
           RETURNING *"#,
    )
    .bind(id)
    .bind(auth_user.user_id)
    .bind(new_status)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::NotFound("Approval not found or already processed".to_string()))?;

    // Notify the member who submitted this approval
    {
        let pool2 = pool.clone();
        let a = approval.clone();
        let reviewer = auth_user.username.clone();
        let event = if new_status == "approved" {
            "approval_approved"
        } else {
            "approval_rejected"
        };
        let status_str = new_status.to_string();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            super::notification::notify_user(
                &pool2,
                a.requested_by,
                a.tenant_id,
                event,
                &format!("Your request was {}", status_str),
                &format!("{} by {}", status_str, reviewer),
                serde_json::json!({"approval_id": a.id, "command": a.command, "reviewer": reviewer, "status": status_str}),
                Some(a.id),
                Some(&tx2),
            )
            .await;
        });
    }

    // Spawn background execution when approved
    if new_status == "approved" {
        // Transition Jira to "In Progress"
        if let Some(ref jira_key) = approval.jira_key {
            let pool2 = pool.clone();
            let tid = approval.tenant_id;
            let key = jira_key.clone();
            tokio::spawn(async move {
                transition_jira(&pool2, tid, &key, "In Progress").await;
            });
        }

        let pool_clone = pool.clone();
        let approval_clone = approval.clone();
        let tx_clone = tx.clone();
        tokio::spawn(async move {
            execute_approved(&pool_clone, &approval_clone, &tx_clone).await;
        });
    }

    Ok(approval)
}

/// Create a new pending approval. Jira ticket is auto-linked in the background.
pub async fn create(
    pool: &PgPool,
    auth_user: &AuthUser,
    command: &str,
    reason: Option<&str>,
    plan_detail: Option<&serde_json::Value>,
    tx: &broadcast::Sender<Notification>,
) -> AppResult<Approval> {
    let approval = sqlx::query_as::<_, Approval>(
        r#"INSERT INTO approvals (command, reason, requested_by, tenant_id, status, plan_detail)
           VALUES ($1, $2, $3, $4, 'pending', $5)
           RETURNING *"#,
    )
    .bind(command)
    .bind(reason)
    .bind(auth_user.user_id)
    .bind(auth_user.tenant_id)
    .bind(plan_detail)
    .fetch_one(pool)
    .await?;

    tracing::info!(
        "Approval {} created by user {} — command: {}",
        approval.id,
        auth_user.user_id,
        command
    );

    // Create Jira ticket synchronously so the response includes jira_key.
    // The agent needs it immediately to report to the user.
    let mut approval = approval;
    if let Some(jira_key) = auto_create_jira_ticket(pool, auth_user.tenant_id, command, reason).await
        && let Ok(updated) =
            sqlx::query_as::<_, Approval>("UPDATE approvals SET jira_key = $2 WHERE id = $1 RETURNING *")
                .bind(approval.id)
                .bind(&jira_key)
                .fetch_one(pool)
                .await
    {
        approval = updated;
    }

    // Notify tenant admins about new approval request (exclude the submitter)
    {
        let pool2 = pool.clone();
        let a = approval.clone();
        let username = auth_user.username.clone();
        let submitter_id = auth_user.user_id;
        let cmd = command.to_string();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            super::notification::notify_users_with_permission(
                &pool2,
                a.tenant_id,
                "approval.approve",
                "approval_submitted",
                &format!("New approval request from {}", username),
                &cmd,
                serde_json::json!({"approval_id": a.id, "command": cmd, "requester": username}),
                Some(a.id),
                Some(submitter_id),
                Some(&tx2),
            )
            .await;
        });
    }

    Ok(approval)
}

/// Withdraw a pending approval — only the original requester can do this.
pub async fn withdraw(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    tx: &broadcast::Sender<Notification>,
) -> AppResult<Approval> {
    let approval = sqlx::query_as::<_, Approval>(
        r#"UPDATE approvals SET status = 'withdrawn', withdrawn_at = NOW()
           WHERE id = $1 AND requested_by = $2 AND status = 'pending'
           RETURNING *"#,
    )
    .bind(id)
    .bind(auth_user.user_id)
    .fetch_optional(pool)
    .await?
    .ok_or_else(|| AppError::BadRequest("Approval not found or not in pending status".to_string()))?;

    tracing::info!("Approval {} withdrawn by user {}", id, auth_user.user_id);

    // Cancel Jira ticket if linked
    if let Some(ref jira_key) = approval.jira_key {
        let pool2 = pool.clone();
        let tid = approval.tenant_id;
        let key = jira_key.clone();
        tokio::spawn(async move {
            transition_jira(&pool2, tid, &key, "Cancelled").await;
        });
    }

    // Notify tenant admins that the request was withdrawn
    {
        let pool2 = pool.clone();
        let a = approval.clone();
        let username = auth_user.username.clone();
        let submitter_id = auth_user.user_id;
        let tx2 = tx.clone();
        tokio::spawn(async move {
            super::notification::notify_users_with_permission(
                &pool2,
                a.tenant_id,
                "approval.approve",
                "approval_withdrawn",
                &format!("{} withdrew their approval request", username),
                &a.command,
                serde_json::json!({"approval_id": a.id, "command": a.command, "requester": username}),
                Some(a.id),
                Some(submitter_id),
                Some(&tx2),
            )
            .await;
        });
    }

    Ok(approval)
}

/// Link a Jira issue key to an existing approval.
pub async fn update_jira_key(pool: &PgPool, auth_user: &AuthUser, id: Uuid, jira_key: &str) -> AppResult<Approval> {
    // Verify access
    let existing = sqlx::query_as::<_, Approval>("SELECT * FROM approvals WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Approval not found".to_string()))?;

    if !auth_user.is_super_admin() && existing.tenant_id != auth_user.tenant_id {
        return Err(AppError::Forbidden("Access denied".to_string()));
    }

    let approval = sqlx::query_as::<_, Approval>("UPDATE approvals SET jira_key = $2 WHERE id = $1 RETURNING *")
        .bind(id)
        .bind(jira_key)
        .fetch_one(pool)
        .await?;

    tracing::info!("Approval {} linked to Jira {}", id, jira_key);
    Ok(approval)
}

/// Admin marks an approval as success (completed) or failure after reviewing output.
/// Valid for approvals in "approved", "executed", or "failed" status.
pub async fn mark_result(
    pool: &PgPool,
    auth_user: &AuthUser,
    id: Uuid,
    success: bool,
    tx: &broadcast::Sender<Notification>,
) -> AppResult<Approval> {
    if !auth_user.is_admin() {
        return Err(AppError::Forbidden("Only admins can mark approval results".to_string()));
    }

    let existing = sqlx::query_as::<_, Approval>("SELECT * FROM approvals WHERE id = $1")
        .bind(id)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| AppError::NotFound("Approval not found".to_string()))?;

    if !auth_user.is_super_admin() && existing.tenant_id.is_some() && existing.tenant_id != auth_user.tenant_id {
        return Err(AppError::Forbidden("Access denied".to_string()));
    }

    if existing.status != "executed" && existing.status != "failed" && existing.status != "approved" {
        return Err(AppError::BadRequest(format!(
            "Can only mark result for approved/executed/failed approvals, current status: {}",
            existing.status
        )));
    }

    let new_status = if success { "completed" } else { "failed" };

    // Preserve existing output text if any, otherwise use a manual mark note
    let output_text = existing
        .execution_result
        .as_ref()
        .and_then(|r| r.get("output").or_else(|| r.get("error")))
        .and_then(|v| v.as_str())
        .unwrap_or("Manually marked by admin");

    let result_json = if success {
        serde_json::json!({ "output": output_text })
    } else {
        serde_json::json!({ "error": output_text })
    };

    let approval = sqlx::query_as::<_, Approval>(
        "UPDATE approvals SET status = $2, execution_result = $3, marked_by = $4 WHERE id = $1 RETURNING *",
    )
    .bind(id)
    .bind(new_status)
    .bind(&result_json)
    .bind(auth_user.user_id)
    .fetch_one(pool)
    .await?;

    tracing::info!(
        "Approval {} manually marked as {} by admin {}",
        id,
        new_status,
        auth_user.user_id
    );

    // ✅ → transition Jira to Done; ❌ → transition Jira to Failed
    if let Some(ref jira_key) = approval.jira_key {
        let target = if success { "Done" } else { "Failed" };
        let pool2 = pool.clone();
        let tid = approval.tenant_id;
        let key = jira_key.clone();
        tokio::spawn(async move {
            transition_jira(&pool2, tid, &key, target).await;
        });
    }

    // Notify the submitting member about the override
    {
        let pool2 = pool.clone();
        let a = approval.clone();
        let reviewer = auth_user.username.clone();
        let tx2 = tx.clone();
        tokio::spawn(async move {
            let label = if success { "success" } else { "failure" };
            super::notification::notify_user(
                &pool2,
                a.requested_by,
                a.tenant_id,
                "approval_result_overridden",
                &format!("Result marked as {} by {}", label, reviewer),
                &a.command,
                serde_json::json!({"approval_id": a.id, "success": success, "reviewer": reviewer}),
                Some(a.id),
                Some(&tx2),
            )
            .await;
        });
    }

    Ok(approval)
}

/// Execute an approved plan by spawning a Claude CLI subprocess.
/// Updates the approval record with results and posts a Jira comment.
async fn execute_approved(pool: &PgPool, approval: &Approval, tx: &broadcast::Sender<Notification>) {
    let raw_prompt = match &approval.plan_detail {
        Some(detail) => detail["prompt"].as_str().unwrap_or(&approval.command).to_string(),
        None => approval.command.clone(),
    };

    tracing::info!(
        "Executing approved plan {} (jira={:?}): {}",
        approval.id,
        approval.jira_key,
        &raw_prompt[..raw_prompt.len().min(200)]
    );

    // Mark as executing
    if let Err(e) = sqlx::query("UPDATE approvals SET status = 'executing', executed_at = NOW() WHERE id = $1")
        .bind(approval.id)
        .execute(pool)
        .await
    {
        tracing::error!("Failed to mark approval {} as executing: {}", approval.id, e);
    }

    // Load provider credentials + AWS context in parallel
    let (mut env, (aws_env, account_context)) = tokio::join!(
        load_provider_env(pool, approval.tenant_id),
        load_aws_context(pool, approval.requested_by, approval.tenant_id),
    );
    env.extend(aws_env);

    tracing::info!(
        "Approval {} prompt context ({}B): {}",
        approval.id,
        account_context.len(),
        &account_context[..account_context.len().min(500)]
    );

    // Build execution prompt with explicit instructions + auto-verification
    let prompt = format!(
        "You are an infrastructure execution agent. An admin has approved the following operation.\n\n\
         ## Rules\n\
         1. Execute it NOW using the appropriate CLI commands (aws cli, kubectl, terraform, etc.).\n\
         2. Do NOT just describe the steps — actually run them.\n\
         3. If a command fails, include the full error output.\n\n\
         ## Verification (MANDATORY)\n\
         After execution, you MUST verify the result by running a separate check command. Examples:\n\
         - Created S3 bucket → `aws s3api head-bucket --bucket <name>`\n\
         - Created EC2 instance → `aws ec2 describe-instances --instance-ids <id> --query 'Reservations[].Instances[].State'`\n\
         - Created IAM role → `aws iam get-role --role-name <name>`\n\
         - Applied k8s resource → `kubectl get <resource> -n <ns>`\n\
         - Scaled deployment → `kubectl rollout status deployment/<name> -n <ns>`\n\
         Choose the appropriate verification command based on what you just executed.\n\n\
         ## Output Format\n\
         Report in this structure:\n\
         **Execution**: what commands were run and their output\n\
         **Verification**: the check command and its result (PASS / FAIL)\n\
         **Summary**: one-line result\n\n\
         {account_context}\
         ## Approved Operation\n\
         {raw_prompt}"
    );

    // Run Claude CLI
    let result = crate::services::scheduler::execute_prompt(&prompt, &env).await;

    // We intentionally do NOT try to auto-detect success/failure from the output.
    // Claude's natural language output is unreliable for determining whether the
    // actual infrastructure operation succeeded. Instead:
    // - CLI exit 0 → "executed" (neutral: execution completed, admin reviews output)
    // - CLI exit != 0 → "failed" (the agent process itself crashed)
    // Admins can manually mark the final status via the UI after reviewing output.
    let (status, output, result_json) = match result {
        Ok(out) => ("executed", out.clone(), serde_json::json!({ "output": out })),
        Err(err) => ("failed", err.clone(), serde_json::json!({ "error": err })),
    };

    if let Err(e) = sqlx::query("UPDATE approvals SET status = $2, execution_result = $3 WHERE id = $1")
        .bind(approval.id)
        .bind(status)
        .bind(&result_json)
        .execute(pool)
        .await
    {
        tracing::error!("Failed to mark approval {} as {}: {}", approval.id, status, e);
    }

    tracing::info!("Approval {} execution completed (status={})", approval.id, status);

    // Notify the submitting member
    {
        let event = if status == "executed" {
            "approval_executed"
        } else {
            "approval_failed"
        };
        let title = if status == "executed" {
            "Your request executed successfully"
        } else {
            "Your request execution failed"
        };
        let summary = if output.len() > 200 { &output[..200] } else { &output };
        super::notification::notify_user(
            pool,
            approval.requested_by,
            approval.tenant_id,
            event,
            title,
            summary,
            serde_json::json!({"approval_id": approval.id, "command": approval.command, "status": status}),
            Some(approval.id),
            Some(tx),
        )
        .await;
    }

    // Post output to Jira as comment only — no status transition.
    // Admin reviews output and manually marks success/failure via mark_result(),
    // which then transitions Jira to Done or Failed.
    if let Some(ref jira_key) = approval.jira_key {
        post_jira_comment(pool, approval.tenant_id, jira_key, &output).await;
    }
}

/// Extract Jira base URL for a tenant (for link rendering in the UI).
/// Reuses the same channel lookup as `get_jira_client`.
pub async fn get_jira_base_url(pool: &PgPool, tenant_id: Option<Uuid>) -> Option<String> {
    let channel = get_jira_channel(pool, tenant_id).await?;
    channel
        .credentials
        .get("base_url")
        .and_then(|v| v.as_str())
        .map(|s| s.trim_end_matches('/').to_string())
}

/// Get Jira channel for a tenant.
/// Tenant users: strict match via channel_tenants.
/// Super admins (tenant_id=None): pick the first enabled Jira channel.
async fn get_jira_channel(pool: &PgPool, tenant_id: Option<Uuid>) -> Option<Channel> {
    match tenant_id {
        Some(tid) => sqlx::query_as::<_, Channel>(
            r#"SELECT c.* FROM channels c
                   JOIN channel_tenants ct ON ct.channel_id = c.id
                   WHERE c.platform = 'jira' AND c.enabled = true AND ct.tenant_id = $1
                   LIMIT 1"#,
        )
        .bind(tid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten(),
        None => {
            sqlx::query_as::<_, Channel>("SELECT * FROM channels WHERE platform = 'jira' AND enabled = true LIMIT 1")
                .fetch_optional(pool)
                .await
                .ok()
                .flatten()
        }
    }
}

/// Get Jira client for a tenant, if configured.
fn get_jira_client_from_channel(channel: &Channel) -> Option<JiraClient> {
    JiraClient::from_credentials(&channel.credentials)
        .map_err(|e| tracing::warn!("Failed to create Jira client: {}", e))
        .ok()
}

async fn get_jira_client(pool: &PgPool, tenant_id: Option<Uuid>) -> Option<JiraClient> {
    let channel = get_jira_channel(pool, tenant_id).await?;
    get_jira_client_from_channel(&channel)
}

/// Post execution output as a Jira comment (audit trail only, no status transition).
/// The Jira ticket is transitioned to Done only when admin manually marks ✅ via `mark_result`.
async fn post_jira_comment(pool: &PgPool, tenant_id: Option<Uuid>, jira_key: &str, result: &str) {
    let Some(client) = get_jira_client(pool, tenant_id).await else {
        tracing::warn!("No Jira channel for tenant {:?}, skipping comment", tenant_id);
        return;
    };

    let truncated = if result.len() > 8000 {
        let end = result.char_indices().nth(2000).map_or(result.len(), |(i, _)| i);
        format!("{}...", &result[..end])
    } else {
        result.to_string()
    };
    let comment = format!("⚙️ Execution completed — awaiting admin review\n\n{}", truncated);

    if let Err(e) = client.add_comment(jira_key, &comment).await {
        tracing::error!("Failed to add Jira comment to {}: {}", jira_key, e);
    }
}

/// Transition Jira ticket to the given status (e.g. "In Progress", "Done").
async fn transition_jira(pool: &PgPool, tenant_id: Option<Uuid>, jira_key: &str, target: &str) {
    let Some(client) = get_jira_client(pool, tenant_id).await else {
        return;
    };
    if let Err(e) = client.transition_issue(jira_key, target, None).await {
        tracing::debug!("Failed to transition {} to {}: {}", jira_key, target, e);
    }
}

/// Auto-create a Jira ticket for an approval if the tenant has an enabled Jira channel.
/// Returns the Jira issue key (e.g. "OPS-123") on success, None if no channel or on error.
async fn auto_create_jira_ticket(
    pool: &PgPool,
    tenant_id: Option<Uuid>,
    command: &str,
    reason: Option<&str>,
) -> Option<String> {
    let client = get_jira_client(pool, tenant_id).await?;

    let summary = format!("[Ops] {}", command);
    let description = reason.unwrap_or(command);

    match client.create_issue(&summary, description, None, None).await {
        Ok(issue) => {
            tracing::info!("Auto-created Jira ticket {} for approval", issue.key);
            Some(issue.key)
        }
        Err(e) => {
            tracing::warn!("Failed to auto-create Jira ticket: {}", e);
            None
        }
    }
}

/// Load AWS context for approval execution.
/// Returns (env_vars, account_context_string):
/// - env_vars: region/profile for the subprocess
/// - account_context_string: assume-role instructions for the execution prompt
async fn load_aws_context(pool: &PgPool, user_id: Uuid, tenant_id: Option<Uuid>) -> (Vec<(String, String)>, String) {
    // Get account IDs accessible to the requesting user
    let account_ids: Vec<Uuid> = sqlx::query_scalar::<_, Uuid>(
        r#"SELECT DISTINCT id FROM (
            SELECT id FROM cloud_accounts WHERE tenant_id IS NOT DISTINCT FROM $1
            UNION
            SELECT account_id FROM user_account_access WHERE user_id = $2
        ) sub"#,
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if account_ids.is_empty() {
        return (Vec::new(), String::new());
    }

    // Load all accessible AWS accounts (non-mock)
    let accounts = sqlx::query_as::<_, (String, Option<String>, Option<String>, Vec<String>, String)>(
        r#"SELECT name, account_id, role_arn, regions, source FROM cloud_accounts
           WHERE provider = 'aws' AND is_mock = false AND id = ANY($1)
           ORDER BY CASE WHEN source = 'manual' THEN 0 ELSE 1 END, created_at ASC"#,
    )
    .bind(&account_ids)
    .fetch_all(pool)
    .await
    .unwrap_or_default();

    if accounts.is_empty() {
        return (Vec::new(), String::new());
    }

    // First account (manual/hub) provides base env vars
    let mut env = Vec::new();
    if let Some(first_region) = accounts[0].3.first() {
        env.push(("AWS_DEFAULT_REGION".to_string(), first_region.clone()));
    }

    // Build account context for the execution prompt
    let mut ctx = String::from(
        "## AWS Context\n\
         Your runtime has direct AWS permissions for the management account (ReadOnlyAccess + OpsWrite).\n\
         For same-account operations, use AWS CLI directly — no assume-role needed.\n\
         Do NOT use --profile flags.\n\n",
    );

    // List accessible accounts; flag cross-account ones that need assume-role
    let hub_account_id = accounts[0].1.as_deref().unwrap_or("");
    ctx.push_str("Accessible accounts:\n");
    for (name, acct_id, role_arn, regions, _source) in &accounts {
        let id_str = acct_id.as_deref().unwrap_or("unknown");
        let region_str = if regions.is_empty() {
            "us-west-1".to_string()
        } else {
            regions.join(", ")
        };
        let is_cross = acct_id.as_deref().unwrap_or("") != hub_account_id;
        if is_cross {
            let role_str = role_arn.as_deref().filter(|s| !s.is_empty()).unwrap_or("N/A");
            ctx.push_str(&format!(
                "- {name} (Account: {id_str}, Region: {region_str}) ⚠ CROSS-ACCOUNT — must assume-role first:\n\
                 ```bash\n\
                 CREDS=$(aws sts assume-role --role-arn {role_str} --role-session-name opsk-exec --output json)\n\
                 export AWS_ACCESS_KEY_ID=$(echo $CREDS | jq -r .Credentials.AccessKeyId)\n\
                 export AWS_SECRET_ACCESS_KEY=$(echo $CREDS | jq -r .Credentials.SecretAccessKey)\n\
                 export AWS_SESSION_TOKEN=$(echo $CREDS | jq -r .Credentials.SessionToken)\n\
                 ```\n",
            ));
        } else {
            ctx.push_str(&format!(
                "- {name} (Account: {id_str}, Region: {region_str}) ✅ Direct access — use AWS CLI directly\n",
            ));
        }
    }
    ctx.push('\n');

    tracing::info!(
        "Approval execution: {} accessible AWS accounts, env vars: {:?}",
        accounts.len(),
        env.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>()
    );

    (env, ctx)
}

/// Load provider credentials for the tenant's default provider.
/// Returns env vars that let Claude CLI authenticate (API key, Bedrock region, etc.).
async fn load_provider_env(pool: &PgPool, tenant_id: Option<Uuid>) -> Vec<(String, String)> {
    let row = if let Some(tid) = tenant_id {
        sqlx::query_as::<_, (String, serde_json::Value)>(
            r#"SELECT p.provider_type, p.config FROM providers p
               JOIN tenant_providers tp ON p.id = tp.provider_id
               WHERE tp.tenant_id = $1 AND tp.is_default = true
               LIMIT 1"#,
        )
        .bind(tid)
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
    } else {
        sqlx::query_as::<_, (String, serde_json::Value)>(
            "SELECT provider_type, config FROM providers ORDER BY created_at LIMIT 1",
        )
        .fetch_optional(pool)
        .await
        .ok()
        .flatten()
    };

    match row {
        Some((provider_type, config)) => super::claude::build_provider_env_vars(&provider_type, &config),
        None => Vec::new(),
    }
}
