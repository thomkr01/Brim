-- Notes, note edits and staff actions made through MCP name the AI client that wrote them, so staff
-- and later AI reads can tell them from the bot owner's own words (owner decision 2026-11-17). NULL = a person.
ALTER TABLE notes ADD COLUMN ai_client TEXT;
ALTER TABLE note_edits ADD COLUMN ai_client TEXT;
ALTER TABLE mod_actions ADD COLUMN ai_client TEXT;

CREATE OR REPLACE VIEW mcp_notes AS
    SELECT id, short_id, guild_id, user_id, author_id, text, created_at, ai_client FROM notes;
CREATE OR REPLACE VIEW mcp_mod_actions AS
    SELECT id, guild_id, mod_id, kind, source, target_id, case_no, created_at, ai_client FROM mod_actions;
