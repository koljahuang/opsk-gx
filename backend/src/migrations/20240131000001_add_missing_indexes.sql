-- Add missing indexes for frequently queried columns

-- users: OAuth account linking queries (WHERE email = $1)
CREATE INDEX IF NOT EXISTS idx_users_email ON users(email) WHERE email IS NOT NULL;

-- cloud_accounts: discovery/scan queries (WHERE provider='aws' AND is_mock=false)
CREATE INDEX IF NOT EXISTS idx_cloud_accounts_provider_mock ON cloud_accounts(provider, is_mock);

-- issues: list views (ORDER BY created_at DESC)
CREATE INDEX IF NOT EXISTS idx_issues_created ON issues(created_at DESC);

-- findings: dashboard aggregation (WHERE scan_id=$1 AND compliant=false GROUP BY ...)
CREATE INDEX IF NOT EXISTS idx_findings_scan_compliant ON findings(scan_id, compliant);

-- scans: latest scan lookup (ORDER BY completed_at DESC)
CREATE INDEX IF NOT EXISTS idx_scans_completed ON scans(completed_at DESC);

-- user_account_access: reverse lookup by account_id
CREATE INDEX IF NOT EXISTS idx_user_account_access_account ON user_account_access(account_id);

-- approvals: filtered list (WHERE tenant_id=$1 ORDER BY created_at DESC)
CREATE INDEX IF NOT EXISTS idx_approvals_created ON approvals(created_at DESC);

-- scheduled_jobs: scheduler poll (WHERE enabled=true)
CREATE INDEX IF NOT EXISTS idx_scheduled_jobs_enabled ON scheduled_jobs(enabled) WHERE enabled = true;

-- job_runs: per-job history (WHERE job_id=$1 ORDER BY started_at DESC)
CREATE INDEX IF NOT EXISTS idx_job_runs_job_started ON job_runs(job_id, started_at DESC);
