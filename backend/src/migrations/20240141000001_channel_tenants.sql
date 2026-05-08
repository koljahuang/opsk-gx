-- Channel-Tenant many-to-many: one channel can serve multiple tenants.
-- Replaces the old channels.tenant_id single-FK column.

CREATE TABLE IF NOT EXISTS channel_tenants (
    channel_id UUID NOT NULL REFERENCES channels(id) ON DELETE CASCADE,
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    PRIMARY KEY (channel_id, tenant_id)
);
CREATE INDEX IF NOT EXISTS idx_channel_tenants_tenant ON channel_tenants(tenant_id);

-- Migrate existing data (skip if column already dropped)
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM information_schema.columns WHERE table_name = 'channels' AND column_name = 'tenant_id') THEN
    INSERT INTO channel_tenants (channel_id, tenant_id)
    SELECT id, tenant_id FROM channels WHERE tenant_id IS NOT NULL
    ON CONFLICT DO NOTHING;
  END IF;
END $$;

-- Drop old indexes that reference tenant_id (auto-dropped with column, but be explicit)
DROP INDEX IF EXISTS idx_channels_tenant;
DROP INDEX IF EXISTS idx_channels_tenant_name;

-- Drop old column
ALTER TABLE channels DROP COLUMN IF EXISTS tenant_id;

-- Drop global name uniqueness — different tenants may have same-named channels.
DROP INDEX IF EXISTS idx_channels_name;
