-- tenant_providers: many-to-many relationship between tenants and providers (model cards).
-- Providers become global (super_admin creates), then assigned to tenants.

CREATE TABLE IF NOT EXISTS tenant_providers (
    tenant_id UUID NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    provider_id UUID NOT NULL REFERENCES providers(id) ON DELETE CASCADE,
    is_default BOOLEAN NOT NULL DEFAULT false,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tenant_id, provider_id)
);

-- Migrate existing data: copy providers.tenant_id relationships to tenant_providers
INSERT INTO tenant_providers (tenant_id, provider_id, is_default)
SELECT tenant_id, id, is_default FROM providers WHERE tenant_id IS NOT NULL
ON CONFLICT DO NOTHING;

-- Drop tenant_id and is_default from providers (now global)
ALTER TABLE providers DROP COLUMN IF EXISTS is_default;
ALTER TABLE providers DROP COLUMN IF EXISTS tenant_id;
