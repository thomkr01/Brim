-- The members of each server now: set from Discord's count when Brim gets the server, then +1 or
-- -1 on each join and leave. The hourly fold copies it into today's `members_total` count. A
-- number only, no member id; the server purge deletes the row.
CREATE TABLE stat_members (
    guild_id BIGINT PRIMARY KEY,
    n        BIGINT NOT NULL
);
CREATE TRIGGER stat_read_only BEFORE INSERT OR UPDATE OR DELETE ON stat_members
    FOR EACH ROW EXECUTE FUNCTION stat_read_only();
