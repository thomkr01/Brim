-- Giveaway extras: a winner role for a time, a private thread for the winners that closes by
-- itself, entry by reaction instead of the button, and an entry code.
ALTER TABLE giveaways ADD COLUMN extras JSONB NOT NULL DEFAULT '{}';
-- The open winner thread; NULL when there is none or after it closed.
ALTER TABLE giveaways ADD COLUMN thread_id BIGINT;
-- SHA-256 of the entry code (trimmed, lower case), salted with the giveaway id. The code itself is
-- never kept.
ALTER TABLE giveaways ADD COLUMN code_hash TEXT;

-- Wrong entry codes per member: 3 wrong tries give a 10 minute wait. A right code removes the row.
CREATE TABLE giveaway_code_tries (
    giveaway_id  BIGINT NOT NULL REFERENCES giveaways (id) ON DELETE CASCADE,
    guild_id     BIGINT NOT NULL,
    user_id      BIGINT NOT NULL,
    tries        SMALLINT NOT NULL DEFAULT 0,
    locked_until TIMESTAMPTZ,
    PRIMARY KEY (giveaway_id, user_id)
);
-- A data deletion asks by user only.
CREATE INDEX giveaway_code_tries_user ON giveaway_code_tries (user_id);
-- Only the bot counts wrong codes, also when an older image grants the dashboard every row right.
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON giveaway_code_tries
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit();
