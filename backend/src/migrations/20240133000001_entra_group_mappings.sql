-- Entra ID group-to-role mappings
CREATE TABLE IF NOT EXISTS entra_group_mappings (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  group_id TEXT NOT NULL UNIQUE,
  group_name TEXT NOT NULL DEFAULT '',
  role TEXT NOT NULL DEFAULT 'member',
  tenant_id UUID REFERENCES tenants(id) ON DELETE SET NULL,
  account_access JSONB NOT NULL DEFAULT '[]'::jsonb,
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_entra_group_mappings_group_id ON entra_group_mappings(group_id);
