-- Rename to Brim (PR D), two parts.
--
-- 1. The erasure log's skip flag can only be set by the owner (security pass of PR B). Until now
-- any role could `SET brim.erasing = on` itself (or keep it as its own default with ALTER ROLE ... SET),
-- and then its own deletes of grants (Extra Owners, Trusted Admins, permits, rescue key, dashboard 2FA,
-- MCP tokens) were not logged, so a later restore brought them back. Now purge_guild and
-- restore_erasures also write their transaction into an owner-only table, and the log functions skip
-- only when the flag is on AND that row is there. The table is in its own schema, which no other role
-- may use: an older image's dashboard role setup grants every table in `public` again.
-- `at = now` (the transaction's start) belongs to the check: a row restored from another
-- database's dump can carry a transaction id that this database hands out again.
-- The skip is also scoped to the purged server (`guild`; NULL: a restore, every row): the dashboard
-- may call purge_guild, and its other deletes in that same transaction must still be logged (the
-- log triggers are deferred to commit, while the marker still holds).
-- The four functions are those of migration 4054 with only the flag lines changed.
CREATE SCHEMA IF NOT EXISTS brim_owner;
REVOKE ALL ON SCHEMA brim_owner FROM PUBLIC;
CREATE TABLE IF NOT EXISTS brim_owner.erasing (
    xid   XID8 NOT NULL,
    guild BIGINT,
    at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS erasing_xid ON brim_owner.erasing (xid);

-- Called only from purge_guild (SECURITY DEFINER) and restore_erasures (the owner's restore).
-- Rows of finished transactions go; the flag stays on to the end of this one (deferred triggers).
CREATE OR REPLACE FUNCTION brim_owner.erasing_on(g BIGINT) RETURNS void
LANGUAGE plpgsql SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    DELETE FROM brim_owner.erasing WHERE xid < pg_snapshot_xmin(pg_current_snapshot());
    INSERT INTO brim_owner.erasing (xid, guild) VALUES (pg_current_xact_id(), g);
    PERFORM set_config('brim.erasing', 'on', true);
END $$;

-- `g`: the server of the row (NULL for a row of no server, which only a restore skips).
CREATE OR REPLACE FUNCTION brim_owner.erasing(g BIGINT) RETURNS boolean
LANGUAGE sql SET search_path = pg_catalog, pg_temp AS $$
    SELECT current_setting('brim.erasing', true) = 'on'
       AND EXISTS (SELECT 1 FROM brim_owner.erasing WHERE xid = pg_current_xact_id() AND at = now()
                   AND (guild IS NULL OR guild = g))
$$;
REVOKE ALL ON FUNCTION brim_owner.erasing_on(BIGINT), brim_owner.erasing(BIGINT) FROM PUBLIC;

CREATE OR REPLACE FUNCTION note_erasure(t TEXT, k JSONB) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    -- restore_erasures and purge_guild delete many rows: their own deletes are not logged again.
    IF brim_owner.erasing((k ->> 'guild_id')::BIGINT) THEN RETURN; END IF;
    DELETE FROM erasure_log WHERE at < now() - interval '35 days';
    INSERT INTO erasure_log (tbl, key) VALUES (t, k);
END $$;

CREATE OR REPLACE FUNCTION log_erasure() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
DECLARE k JSONB; w TEXT; back BOOLEAN;
BEGIN
    -- Inside a purge or a restore: skip the lookup below for each of its many rows.
    IF brim_owner.erasing((to_jsonb(OLD) ->> 'guild_id')::BIGINT) THEN RETURN NULL; END IF;
    SELECT jsonb_object_agg(c, to_jsonb(OLD) -> c),
           string_agg(CASE WHEN to_jsonb(OLD) ->> c IS NULL THEN format('%I IS NULL', c)
                           ELSE format('%1$I = ($1).%1$I', c) END, ' AND ')
      INTO k, w FROM unnest(TG_ARGV) c;
    -- Plain column comparisons, so the lookup uses the table's key index.
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I WHERE %s)', TG_TABLE_NAME, w) INTO back USING OLD;
    IF NOT back THEN PERFORM note_erasure(TG_TABLE_NAME, k); END IF;
    RETURN NULL;
END $$;

CREATE OR REPLACE FUNCTION purge_guild(g BIGINT) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
DECLARE t TEXT;
BEGIN
    PERFORM note_erasure('purge_guild', jsonb_build_object('guild_id', g));
    PERFORM brim_owner.erasing_on(g);
    FOR t IN
        SELECT c.table_name FROM information_schema.columns c
        JOIN information_schema.tables x USING (table_schema, table_name)
        WHERE c.table_schema = 'public' AND c.column_name = 'guild_id'
          AND x.table_type = 'BASE TABLE' AND c.table_name <> '_sqlx_migrations'
    LOOP
        EXECUTE format('DELETE FROM %I WHERE guild_id = $1', t) USING g;
    END LOOP;
    UPDATE ticket_users SET last_guild = NULL WHERE last_guild = g;
    PERFORM pg_notify('config_changed', g::text);
END $$;

CREATE OR REPLACE FUNCTION restore_erasures() RETURNS BIGINT
LANGUAGE plpgsql SET search_path = public, pg_temp AS $$
DECLARE m BIGINT; r RECORD; n BIGINT := 0; rid BIGINT;
BEGIN
    SELECT coalesce(max(id), 0) INTO m FROM erasure_log;
    INSERT INTO erasure_log SELECT * FROM saved WHERE id > m;
    PERFORM setval(pg_get_serial_sequence('erasure_log', 'id'), max(id)) FROM erasure_log HAVING max(id) > 0;
    PERFORM brim_owner.erasing_on(NULL);
    FOR r IN SELECT id, tbl, key FROM erasure_log WHERE id > m ORDER BY id LOOP
        -- Only the tables this file logs: a planted row cannot name another table.
        IF r.tbl NOT IN ('purge_guild', 'data_request', 'extra_owners', 'trusted_admins', 'permits', 'permit_members',
                         'permit_roles', 'raider_allow', 'rescue_key', 'dash_totp', 'mcp_tokens') THEN
            RAISE EXCEPTION 'erasure_log row % names an unknown table %', r.id, r.tbl;
        END IF;
        IF r.tbl = 'purge_guild' THEN
            PERFORM purge_guild((r.key ->> 'guild_id')::BIGINT);
        ELSIF r.tbl = 'data_request' AND to_regclass('public.data_requests') IS NULL THEN
            -- A dump from before migration 4034 has no requests table: the owner runs it again by hand.
            RAISE WARNING 'not queued again (dump older than data requests): the data deletion of user %', r.key ->> 'user_id';
        ELSIF r.tbl = 'data_request' THEN
            -- The erasure itself is the bot's code (`core::persondata`): queue its job again, approved by
            -- the bot owner who approved it. The bot runs it only while that person is still the bot
            -- owner; otherwise it goes back to the Data requests page for the new owner.
            UPDATE data_requests SET status = 'approved', decided_by = (r.key ->> 'by')::BIGINT,
                   decided_at = now(), done_at = NULL
             WHERE user_id = (r.key ->> 'user_id')::BIGINT AND kind = 'delete' AND status IN ('pending', 'approved')
            RETURNING id INTO rid;
            IF rid IS NULL THEN
                INSERT INTO data_requests (user_id, username, kind, status, decided_by, decided_at)
                VALUES ((r.key ->> 'user_id')::BIGINT, '(again after a restore)', 'delete', 'approved',
                        (r.key ->> 'by')::BIGINT, now())
                RETURNING id INTO rid;
            END IF;
            INSERT INTO scheduled_jobs (guild_id, kind, payload, run_at)
            VALUES (0, 'data_request', jsonb_build_object('id', rid, 'by', r.key ->> 'by'), now());
            rid := NULL;
        ELSIF r.tbl = 'mcp_tokens' THEN
            -- mcp_writes keeps a reference to the token: revoke it instead of deleting it.
            UPDATE mcp_tokens t SET revoked_at = now() WHERE to_jsonb(t) @> r.key;
        ELSE
            EXECUTE format('DELETE FROM %I t WHERE to_jsonb(t) @> $1', r.tbl) USING r.key;
        END IF;
        n := n + 1;
    END LOOP;
    DELETE FROM dash_session;
    RETURN n;
END $$;

-- 2. The dashboard's role is `brim_dashboard` on a server that moved to the Brim names and
-- `tommybot_dashboard` until then (`db::roles`). The checks of migrations 4035 and 4043 named only
-- the old role; they now name both. Function bodies as in 4043, only the role check changed.
CREATE OR REPLACE FUNCTION dashboard_role_limit() RETURNS trigger LANGUAGE plpgsql
    SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    IF current_user NOT IN ('brim_dashboard', 'tommybot_dashboard') THEN
        RETURN NEW;
    END IF;
    IF TG_TABLE_NAME = 'case_proofs' THEN
        IF TG_OP = 'INSERT' AND NOT NEW.verified AND NEW.channel_id IS NULL
           AND NEW.message_id IS NULL AND NEW.sent_at IS NULL AND NEW.files = '{}'
           AND NEW.added_at = now() AND NEW.id = currval('public.case_proofs_id_seq') THEN
            RETURN NEW;
        END IF;
    ELSIF TG_OP = 'UPDATE' AND TG_NARGS > 0
          AND to_jsonb(NEW) - TG_ARGV = to_jsonb(OLD) - TG_ARGV THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'permission denied: the dashboard may not % this row of %', lower(TG_OP), TG_TABLE_NAME
        USING ERRCODE = '42501';
END $$;

CREATE OR REPLACE FUNCTION dashboard_role_append_only() RETURNS trigger LANGUAGE plpgsql
    SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    IF current_user NOT IN ('brim_dashboard', 'tommybot_dashboard') THEN
        IF TG_OP = 'DELETE' THEN
            RETURN OLD;
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'INSERT'
       AND NEW.id = currval(pg_get_serial_sequence(format('%I.%I', TG_TABLE_SCHEMA, TG_TABLE_NAME), 'id'))
       AND (to_jsonb(NEW) ->> TG_ARGV[0])::timestamptz = now() THEN
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'permission denied: the dashboard may not % this row of %', lower(TG_OP), TG_TABLE_NAME
        USING ERRCODE = '42501';
END $$;

DO $$ BEGIN
    IF to_regclass('public.data_requests') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS erasure_log ON data_requests;
        CREATE TRIGGER erasure_log AFTER UPDATE OF status ON data_requests FOR EACH ROW
            WHEN (NEW.kind = 'delete' AND NEW.status = 'done' AND OLD.status <> 'done'
                  AND current_user NOT IN ('brim_dashboard', 'tommybot_dashboard'))
            EXECUTE FUNCTION log_data_request();
    END IF;
END $$;
