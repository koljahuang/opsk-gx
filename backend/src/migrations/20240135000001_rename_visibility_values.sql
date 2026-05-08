-- Rename visibility values: public → tenant, private → user
-- More semantically clear about scope ownership

UPDATE skills SET visibility = 'tenant' WHERE visibility = 'public';
UPDATE skills SET visibility = 'user' WHERE visibility = 'private';

UPDATE mcp_servers SET visibility = 'tenant' WHERE visibility = 'public';
UPDATE mcp_servers SET visibility = 'user' WHERE visibility = 'private';

UPDATE scheduled_jobs SET visibility = 'tenant' WHERE visibility = 'public';
UPDATE scheduled_jobs SET visibility = 'user' WHERE visibility = 'private';
