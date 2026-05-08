-- Performance indexes for high-frequency queries

-- Notification unread count: SELECT COUNT(*) WHERE user_id = $1 AND NOT is_read
CREATE INDEX IF NOT EXISTS idx_notifications_user_unread
    ON notifications(user_id) WHERE NOT is_read;

-- Notification list: SELECT * WHERE user_id = $1 ORDER BY created_at DESC
CREATE INDEX IF NOT EXISTS idx_notifications_user_created
    ON notifications(user_id, created_at DESC);

-- Issues by status + RCA state (for auto-RCA checks and dashboard queries)
CREATE INDEX IF NOT EXISTS idx_issues_status_rca
    ON issues(status, rca_started_at);

-- Issues by source + status (for alert deduplication in upsert_issue)
CREATE INDEX IF NOT EXISTS idx_issues_source_status
    ON issues(source, status) WHERE status != 'resolved';

-- Deployment events by cluster (for list_events queries)
CREATE INDEX IF NOT EXISTS idx_deployment_events_cluster_created
    ON deployment_events(cluster_id, created_at DESC);
