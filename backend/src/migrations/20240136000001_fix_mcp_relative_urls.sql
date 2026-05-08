-- Convert hardcoded localhost MCP URLs to relative paths
UPDATE mcp_servers
SET url = REGEXP_REPLACE(url, '^https?://[^/]+', '')
WHERE url LIKE 'http://localhost:%'
   OR url LIKE 'http://127.0.0.1:%';
