-- Restore tenant_admin as a distinct role between super_admin and member.
-- No automatic upgrades — admin must manually promote users via the Users page.
-- This migration is a no-op structurally; the role column already accepts any string.
-- It exists as documentation that tenant_admin is a valid role value again.

-- Ensure default remains 'member' for new users
ALTER TABLE users ALTER COLUMN role SET DEFAULT 'member';
