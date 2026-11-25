-- Stats: counts of what Brim and staff do, per server and day, for the stats pages.
-- Owner decision: the counts are kept for good and hold no member data. Only the detail rows
-- (`stat_events`) are pruned, after 90 days, and that never changes a count.
-- `kind` and `feature` are free names (a new kind needs no migration), checked for shape only.

-- One row per event, for hourly graphs and the recent list. No member id and no text.
CREATE TABLE stat_events (
    id       BIGSERIAL PRIMARY KEY,
    guild_id BIGINT NOT NULL,
    at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    kind     TEXT NOT NULL CHECK (kind ~ '^[a-z][a-z0-9_]{0,39}$'),
    feature  TEXT NOT NULL CHECK (feature ~ '^[a-z][a-z0-9_]{0,39}$'),
    -- How many: accounts caught in a raid, messages purged. A correction can be negative.
    n        BIGINT NOT NULL DEFAULT 1,
    -- The moderation act the event belongs to, when there is one.
    case_no  INT
);
CREATE INDEX stat_events_guild_at ON stat_events (guild_id, at);
CREATE INDEX stat_events_at ON stat_events (at);

-- The counts per server and UTC day. Written in the same statement as the event. Never pruned:
-- only the server purge (30 days after Brim leaves) deletes a server's rows, and by then
-- `stat_daily_all` holds them.
CREATE TABLE stat_daily (
    guild_id BIGINT NOT NULL,
    day      DATE NOT NULL,
    kind     TEXT NOT NULL,
    feature  TEXT NOT NULL,
    n        BIGINT NOT NULL,
    PRIMARY KEY (guild_id, day, kind, feature)
);
CREATE INDEX stat_daily_day ON stat_daily (day);

-- The counts of all servers per UTC day, for the bot owner. No server id, so a server that
-- left stays in the totals (owner decision). Filled hourly from `stat_daily` for finished days
-- (`stats::fold`), plus `servers_total`, the number of servers Brim is in, set each hour.
CREATE TABLE stat_daily_all (
    day     DATE NOT NULL,
    kind    TEXT NOT NULL,
    feature TEXT NOT NULL,
    n       BIGINT NOT NULL,
    -- How many servers had this kind that day.
    servers INT NOT NULL DEFAULT 0,
    PRIMARY KEY (day, kind, feature)
);

-- The last day `stats::fold` added to `stat_daily_all` (one row).
CREATE TABLE stat_folded (
    one  BOOLEAN PRIMARY KEY DEFAULT true CHECK (one),
    day  DATE NOT NULL
);
-- Days before this migration have no rows; folding starts from yesterday.
INSERT INTO stat_folded (day) VALUES (((now() AT TIME ZONE 'UTC')::date) - 1);

-- Adds one event and its count, the one write path. SECURITY DEFINER: the dashboard role has no
-- write right on these tables, but its staff actions (`mod_actions`) still count through the trigger.
CREATE FUNCTION stat_add(g BIGINT, k TEXT, f TEXT, cnt BIGINT, c INT) RETURNS void
LANGUAGE sql SECURITY DEFINER SET search_path = public, pg_temp AS $$
    WITH e AS (
        INSERT INTO stat_events (guild_id, kind, feature, n, case_no) VALUES (g, k, f, cnt, c)
        RETURNING guild_id, (at AT TIME ZONE 'UTC')::date AS day, kind, feature, n
    )
    INSERT INTO stat_daily (guild_id, day, kind, feature, n) SELECT guild_id, day, kind, feature, n FROM e
    ON CONFLICT (guild_id, day, kind, feature) DO UPDATE SET n = stat_daily.n + EXCLUDED.n
$$;
REVOKE ALL ON FUNCTION stat_add(BIGINT, TEXT, TEXT, BIGINT, INT) FROM PUBLIC;

-- Every staff and Brim action of moderator stats is also a count. `feature` says which part of
-- Brim did it; a row without one (the dashboard, an older image) counts under its source.
ALTER TABLE mod_actions ADD COLUMN feature TEXT;
CREATE FUNCTION stat_mod_action() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    -- A count must never undo the action it counts: a bad name is only logged.
    BEGIN
        PERFORM stat_add(NEW.guild_id, NEW.kind, coalesce(NEW.feature, NEW.source), 1, NEW.case_no);
    EXCEPTION WHEN OTHERS THEN
        RAISE WARNING 'stats: % not counted: %', NEW.kind, SQLERRM;
    END;
    RETURN NULL;
END $$;
REVOKE ALL ON FUNCTION stat_mod_action() FROM PUBLIC;
-- AFTER INSERT: a replayed audit entry (ON CONFLICT DO NOTHING) inserts nothing and counts nothing.
CREATE TRIGGER stat_mod_action AFTER INSERT ON mod_actions
    FOR EACH ROW EXECUTE FUNCTION stat_mod_action();

-- The dashboard only reads the counts, also after an older image grants it every right again
-- (a rollback). DELETE too: the owner totals are kept for good. The owner's own deletes (the
-- server purge, the prune) pass.
CREATE FUNCTION stat_read_only() RETURNS trigger
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    IF current_user IN ('brim_dashboard', 'tommybot_dashboard', 'brim_mcp', 'tommybot_mcp') THEN
        RAISE EXCEPTION 'permission denied: the dashboard may not % rows of %', lower(TG_OP), TG_TABLE_NAME
            USING ERRCODE = '42501';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER stat_read_only BEFORE INSERT OR UPDATE OR DELETE ON stat_events
    FOR EACH ROW EXECUTE FUNCTION stat_read_only();
CREATE TRIGGER stat_read_only BEFORE INSERT OR UPDATE OR DELETE ON stat_daily
    FOR EACH ROW EXECUTE FUNCTION stat_read_only();
CREATE TRIGGER stat_read_only BEFORE INSERT OR UPDATE OR DELETE ON stat_daily_all
    FOR EACH ROW EXECUTE FUNCTION stat_read_only();
CREATE TRIGGER stat_read_only BEFORE INSERT OR UPDATE OR DELETE ON stat_folded
    FOR EACH ROW EXECUTE FUNCTION stat_read_only();

-- A server's counts that are not in the owner totals yet go there before the server purge
-- deletes them (a server owner may erase the server's data while Brim is still in it), so a
-- server that is gone stays in the totals (owner decision).
CREATE FUNCTION stat_daily_keep() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    INSERT INTO stat_daily_all (day, kind, feature, n, servers)
    SELECT OLD.day, OLD.kind, OLD.feature, OLD.n, 1 FROM stat_folded f WHERE OLD.day > f.day
    ON CONFLICT (day, kind, feature) DO UPDATE
      SET n = stat_daily_all.n + EXCLUDED.n, servers = stat_daily_all.servers + 1;
    RETURN OLD;
END $$;
REVOKE ALL ON FUNCTION stat_daily_keep() FROM PUBLIC;
CREATE TRIGGER stat_daily_keep BEFORE DELETE ON stat_daily
    FOR EACH ROW EXECUTE FUNCTION stat_daily_keep();

-- Brim leaving a server counts once: a second mark of the same leave inserts nothing.
-- A wrong mark that is cleared again counts -1 (`welcome::on_guild_create`, `purge_removed`).
CREATE FUNCTION stat_server_left() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    BEGIN
        PERFORM stat_add(NEW.guild_id, 'server_leave', 'bot', 1, NULL);
    EXCEPTION WHEN OTHERS THEN
        RAISE WARNING 'stats: server_leave not counted: %', SQLERRM;
    END;
    RETURN NULL;
END $$;
REVOKE ALL ON FUNCTION stat_server_left() FROM PUBLIC;
CREATE TRIGGER stat_server_left AFTER INSERT ON guild_left
    FOR EACH ROW EXECUTE FUNCTION stat_server_left();
