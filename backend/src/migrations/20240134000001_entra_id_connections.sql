-- Entra ID enterprise SSO connections
CREATE TABLE IF NOT EXISTS entra_id_connections (
  id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
  name TEXT NOT NULL,
  entra_tenant_id TEXT NOT NULL UNIQUE,
  client_id TEXT NOT NULL,
  client_secret TEXT NOT NULL,
  tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  auto_provision BOOLEAN NOT NULL DEFAULT true,
  default_role TEXT NOT NULL DEFAULT 'member',
  enabled BOOLEAN NOT NULL DEFAULT true,
  allowed_domains TEXT[] NOT NULL DEFAULT '{}',
  created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
  updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_entra_id_connections_entra_tenant_id ON entra_id_connections(entra_tenant_id);
CREATE INDEX IF NOT EXISTS idx_entra_id_connections_tenant_id ON entra_id_connections(tenant_id);
CREATE INDEX IF NOT EXISTS idx_entra_id_connections_allowed_domains ON entra_id_connections USING GIN (allowed_domains);

-- Track which connection initiated an OAuth flow
ALTER TABLE oauth_states ADD COLUMN IF NOT EXISTS connection_id UUID REFERENCES entra_id_connections(id) ON DELETE SET NULL;
