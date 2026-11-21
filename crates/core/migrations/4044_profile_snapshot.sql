-- Logs: a member's last seen username, display name and avatar, so the avatar and name change
-- logs can show the old value (Discord sends only the new one). Written by the bot only, and only
-- while the server sends one of the two logs somewhere. A row goes when the member leaves, 90
-- days after the member was last seen (`bot::retention`), with the person's data delete request, and with
-- the server (purge_guild finds every guild_id column).
CREATE TABLE profile_snapshot (
    guild_id    BIGINT NOT NULL,
    user_id     BIGINT NOT NULL,
    username    TEXT NOT NULL,
    global_name TEXT,
    avatar_hash TEXT,
    -- When the member was last seen in the server: a join, a change, or the daily member list read.
    seen_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, user_id)
);
-- One person's rows across all servers (their data request).
CREATE INDEX profile_snapshot_person ON profile_snapshot (user_id);
-- The hourly 90-day sweep.
CREATE INDEX profile_snapshot_seen ON profile_snapshot (seen_at);

-- The dashboard only reads it (the row count of a data request): the same limit as the bot's other
-- records in migration 4043, also when an older image grants the role every row right again.
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON profile_snapshot
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit();
