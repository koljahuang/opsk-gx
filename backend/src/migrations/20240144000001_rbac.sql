-- RBAC: roles table with permission arrays
CREATE TABLE IF NOT EXISTS roles (
    name        VARCHAR(32) PRIMARY KEY,
    label       VARCHAR(64) NOT NULL,
    description TEXT NOT NULL DEFAULT '',
    permissions JSONB NOT NULL DEFAULT '[]',
    is_system   BOOLEAN NOT NULL DEFAULT false,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Seed system roles
INSERT INTO roles (name, label, description, permissions, is_system) VALUES
('super_admin', 'Super Admin', 'Full platform access', '["*"]', true),
('tenant_admin', 'Tenant Admin', 'Full tenant management',
 '["approval.submit","approval.view_own","approval.view_tenant","approval.approve","approval.mark","approval.withdraw","user.view","user.manage","user.invite","account.view","account.manage","cluster.view","cluster.manage","channel.view","channel.manage","resource.view","resource.scan","topology.view","rollout.view","rollout.manage","provider.view"]', true),
('operator', 'Operator', 'Operations engineer',
 '["approval.submit","approval.view_own","approval.approve","approval.withdraw","account.view","cluster.view","resource.view","topology.view","rollout.view","rollout.manage"]', true),
('member', 'Member', 'Regular team member',
 '["approval.submit","approval.view_own","approval.withdraw","account.view","cluster.view","resource.view","topology.view","rollout.view"]', true),
('viewer', 'Viewer', 'Read-only access',
 '["approval.view_own","account.view","cluster.view","resource.view","topology.view","rollout.view"]', true)
ON CONFLICT (name) DO NOTHING;

-- FK from users.role → roles.name (existing data already uses these role names)
DO $$ BEGIN
    ALTER TABLE users ADD CONSTRAINT fk_users_role FOREIGN KEY (role) REFERENCES roles(name);
EXCEPTION WHEN duplicate_object THEN NULL;
END $$;

-- Approval audit fields
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS marked_by UUID REFERENCES users(id);
ALTER TABLE approvals ADD COLUMN IF NOT EXISTS withdrawn_at TIMESTAMPTZ;
