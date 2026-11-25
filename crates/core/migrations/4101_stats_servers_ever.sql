-- Every server Brim was ever in, for the "servers ever" count. Only a hash of the server id and
-- the first day: no guild_id column, so the server purge keeps the row and the count stays right.
CREATE TABLE stat_servers_ever (
    key       BYTEA PRIMARY KEY,
    first_day DATE NOT NULL
);
CREATE TRIGGER stat_read_only BEFORE INSERT OR UPDATE OR DELETE ON stat_servers_ever
    FOR EACH ROW EXECUTE FUNCTION stat_read_only();

-- The servers Brim knows from before the stats: what is left of them (a server is purged 30 days
-- after Brim leaves it), with the earliest day any of these tables saw it.
INSERT INTO stat_servers_ever (key, first_day)
SELECT sha256(guild_id::text::bytea), min(d)::date FROM (
    SELECT guild_id, seen_at AS d FROM guild_owners
    UNION ALL SELECT guild_id, sent_at FROM welcomed_guilds
    UNION ALL SELECT guild_id, created_at FROM guild_config
    UNION ALL SELECT guild_id, left_at FROM guild_left
) s GROUP BY guild_id
ON CONFLICT DO NOTHING;
