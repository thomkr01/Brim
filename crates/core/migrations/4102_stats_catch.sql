-- The golden channel's caught counter (catch_hits, migration 4096) also counts in the stats, as
-- kind `catch` (one per account caught). Its ban or softban counts as an action with feature
-- `catch` through `mod_actions`.
CREATE FUNCTION stat_catch_hit() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    BEGIN
        PERFORM stat_add(NEW.guild_id, 'catch', 'catch', 1, NULL);
    EXCEPTION WHEN OTHERS THEN
        RAISE WARNING 'stats: catch not counted: %', SQLERRM;
    END;
    RETURN NULL;
END $$;
REVOKE ALL ON FUNCTION stat_catch_hit() FROM PUBLIC;
CREATE TRIGGER stat_catch_hit AFTER INSERT ON catch_hits
    FOR EACH ROW EXECUTE FUNCTION stat_catch_hit();

-- The catches from before: into the per server counts, and the days already folded also into
-- the owner totals (the hourly fold only adds days after `stat_folded`).
INSERT INTO stat_daily (guild_id, day, kind, feature, n)
SELECT guild_id, (at AT TIME ZONE 'UTC')::date, 'catch', 'catch', count(*) FROM catch_hits
GROUP BY 1, 2
ON CONFLICT (guild_id, day, kind, feature) DO UPDATE SET n = stat_daily.n + EXCLUDED.n;
INSERT INTO stat_daily_all (day, kind, feature, n, servers)
SELECT (at AT TIME ZONE 'UTC')::date, 'catch', 'catch', count(*), count(DISTINCT guild_id) FROM catch_hits
WHERE (at AT TIME ZONE 'UTC')::date <= (SELECT day FROM stat_folded)
GROUP BY 1
ON CONFLICT (day, kind, feature) DO UPDATE
  SET n = stat_daily_all.n + EXCLUDED.n, servers = stat_daily_all.servers + EXCLUDED.servers;
