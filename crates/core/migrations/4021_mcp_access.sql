-- MCP access (security review, owner decision 2026-11-17).

-- Each MCP read of a server leaves an entry its staff see on the dashboard History page.
-- The MCP role may only add rows; it cannot read, change or delete them. Pruned after 90 days by the
-- bot's hourly prune; purge_guild deletes a removed server's rows (it finds every guild_id column).
CREATE TABLE mcp_reads (
    id BIGSERIAL PRIMARY KEY,
    guild_id BIGINT NOT NULL,
    tool TEXT NOT NULL CHECK (length(tool) <= 40),
    via TEXT NOT NULL CHECK (length(via) <= 60),
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX mcp_reads_guild ON mcp_reads (guild_id, at DESC);
CREATE INDEX mcp_reads_at ON mcp_reads (at);
-- Only these columns: an own id or time could block the real rows or push them off the page.
GRANT INSERT (guild_id, tool, via) ON mcp_reads TO tommybot_mcp;
GRANT USAGE ON SEQUENCE mcp_reads_id_seq TO tommybot_mcp;

-- The MCP server takes tokens made on the dashboard instead of a fixed MCP_TOKEN. It gets no
-- view of mcp_tokens: it can only ask for the name of the live token with a given hash.
CREATE FUNCTION mcp_token_name(hash BYTEA) RETURNS TEXT
    LANGUAGE sql STABLE SECURITY DEFINER SET search_path = pg_catalog, public, pg_temp
    AS $$ SELECT name FROM public.mcp_tokens WHERE token_hash = hash AND revoked_at IS NULL $$;
REVOKE ALL ON FUNCTION mcp_token_name(BYTEA) FROM PUBLIC;
GRANT EXECUTE ON FUNCTION mcp_token_name(BYTEA) TO tommybot_mcp;

-- The AI no longer sees NSFW scan rows. The view stays (migrations only add), unreadable to the role.
REVOKE SELECT ON mcp_nsfw_scans FROM tommybot_mcp;
