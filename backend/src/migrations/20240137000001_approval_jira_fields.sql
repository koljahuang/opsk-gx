-- Add Jira integration fields to approvals table for webhook-based approval flow
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS jira_key VARCHAR(50);
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS plan_detail JSONB;

CREATE INDEX IF NOT EXISTS idx_approvals_jira_key ON approvals(jira_key);
