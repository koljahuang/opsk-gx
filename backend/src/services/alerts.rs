use sqlx::PgPool;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use tokio::sync::broadcast;

use crate::config::AppConfig;
use crate::models::notification::Notification;
use crate::services::rca::RcaRegistry;

/// Context needed for auto-triggering RCA on critical/high alerts.
/// Avoids depending on AppState in the service layer.
pub struct RcaContext {
    pub pool: PgPool,
    pub registry: Arc<RcaRegistry>,
    pub config: Arc<AppConfig>,
    pub notification_tx: broadcast::Sender<Notification>,
    pub auto_rca_enabled: Arc<AtomicBool>,
}

/// Deduplicate + create/resolve an issue from any alert source.
/// Returns (created_count, resolved_count).
#[allow(clippy::too_many_arguments)]
pub async fn upsert_issue(
    pool: &PgPool,
    source: &str,
    dedup_key: &str,
    title: &str,
    description: &str,
    severity: &str,
    meta: &serde_json::Value,
    is_resolved: bool,
    issue_type: &str,
    rca_context: Option<&RcaContext>,
) -> (u64, u64) {
    let mut created = 0u64;
    let mut resolved = 0u64;

    if is_resolved {
        let result = sqlx::query(
            r#"UPDATE issues SET status = 'resolved', resolved_at = NOW(), updated_at = NOW()
               WHERE source = $1 AND status != 'resolved'
               AND rca_result @> $2::jsonb"#,
        )
        .bind(source)
        .bind(serde_json::json!({"fingerprint": dedup_key}).to_string())
        .execute(pool)
        .await;
        if let Ok(r) = result {
            resolved = r.rows_affected();
        }
        return (created, resolved);
    }

    // Skip duplicate
    let existing = sqlx::query_scalar::<_, i64>(
        r#"SELECT COUNT(*) FROM issues
           WHERE source = $1 AND status != 'resolved'
           AND rca_result @> $2::jsonb"#,
    )
    .bind(source)
    .bind(serde_json::json!({"fingerprint": dedup_key}).to_string())
    .fetch_one(pool)
    .await
    .unwrap_or(0);

    if existing > 0 {
        tracing::debug!("Skipping duplicate {} alert: key={}", source, dedup_key);
        return (created, resolved);
    }

    let result = sqlx::query_scalar::<_, uuid::Uuid>(
        r#"INSERT INTO issues (title, description, source, severity, status, rca_result, issue_type)
           VALUES ($1, $2, $3, $4, 'open', $5, $6)
           RETURNING id"#,
    )
    .bind(title)
    .bind(description)
    .bind(source)
    .bind(severity)
    .bind(meta)
    .bind(issue_type)
    .fetch_one(pool)
    .await;

    match result {
        Ok(issue_id) => {
            created = 1;
            tracing::info!(
                "Created issue {} from {} alert: title={}, severity={}",
                issue_id,
                source,
                title,
                severity
            );

            // Notify tenant admins about the new issue
            if let Some(ctx) = rca_context {
                let ntx = ctx.notification_tx.clone();
                let pool_n = ctx.pool.clone();
                let title_n = title.to_string();
                let severity_n = severity.to_string();
                let source_n = source.to_string();
                let desc_n = format!("[{}] {} — {}", source_n, title_n, severity_n);
                tokio::spawn(async move {
                    crate::services::notification::notify_tenant_admins(
                        &pool_n,
                        None,
                        "issue_created",
                        &title_n,
                        &desc_n,
                        serde_json::json!({
                            "severity": severity_n,
                            "source": source_n,
                        }),
                        Some(issue_id),
                        None,
                        Some(&ntx),
                    )
                    .await;
                });
            }

            // Auto-pause progressing rollouts on critical/high alerts
            if (severity == "critical" || severity == "high")
                && let Some(namespace) = crate::services::rollout_guard::extract_namespace_from_alert(meta)
            {
                let pool_clone = pool.clone();
                let ns = namespace.clone();
                tokio::spawn(async move {
                    let paused = crate::services::rollout_guard::check_and_pause_rollouts(&pool_clone, &ns).await;
                    if !paused.is_empty() {
                        tracing::warn!("Alert guard auto-paused rollouts: {:?}", paused);
                    }
                });
            }

            // Auto-trigger RCA on new issues (if enabled) — all severities
            if let Some(ctx) = rca_context.filter(|c| c.auto_rca_enabled.load(std::sync::atomic::Ordering::Relaxed)) {
                let pool_clone = ctx.pool.clone();
                let registry = ctx.registry.clone();
                let config = ctx.config.clone();
                let ntx = ctx.notification_tx.clone();
                tokio::spawn(async move {
                    let issue = sqlx::query_as::<_, crate::models::issue::Issue>("SELECT * FROM issues WHERE id = $1")
                        .bind(issue_id)
                        .fetch_optional(&pool_clone)
                        .await;

                    if let Ok(Some(issue)) = issue {
                        tracing::info!("Auto-triggering RCA for issue: {}", issue.id);
                        crate::services::rca::run_rca(pool_clone, config, registry, issue, Some(ntx)).await;
                    }
                });
            }
        }
        Err(e) => {
            tracing::error!("Failed to create issue from {} alert: {}", source, e);
        }
    }

    (created, resolved)
}

/// Normalize severity string from various providers to our enum.
pub fn normalize_severity(raw: &str) -> &'static str {
    match raw.to_lowercase().as_str() {
        "critical" | "p1" | "availability" => "critical",
        "high" | "warning" | "p2" | "error" | "resource_contention" => "high",
        "low" | "info" | "p4" | "p5" | "custom_alert" => "low",
        _ => "medium",
    }
}
