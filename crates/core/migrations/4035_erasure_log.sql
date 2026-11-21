-- security review (owner decision 2026-11-17): after a database restore, the deletions made after the dump
-- are applied again. Each grant that is gone when its transaction commits (a removed Extra Owner,
-- Trusted Admin, permit, permit member or permit role, a removed Raider List allow, a replaced or
-- deleted rescue key, a reset dashboard 2FA, a revoked MCP token), each purged server and each
-- person's data deletion the bot carried out leaves one row here. scripts/restore.sh carries the rows newer than the dump into the restored database and
-- calls restore_erasures. Only ids and key hashes, no names, secrets or content; kept 35 days
-- (off-site dumps live about 31). No guild_id column, so purge_guild keeps the rows.
-- scripts/restore-sql.sh runs this file again after a dump from before it.
CREATE TABLE IF NOT EXISTS erasure_log (
    id  BIGSERIAL PRIMARY KEY,
    at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    tbl TEXT NOT NULL,
    key JSONB NOT NULL
);
CREATE INDEX IF NOT EXISTS erasure_log_at ON erasure_log (at);

CREATE OR REPLACE FUNCTION note_erasure(t TEXT, k JSONB) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    -- restore_erasures and purge_guild delete many rows: their own deletes are not logged again.
    IF current_setting('tommybot.erasing', true) = 'on' THEN RETURN; END IF;
    DELETE FROM erasure_log WHERE at < now() - interval '35 days';
    INSERT INTO erasure_log (tbl, key) VALUES (t, k);
END $$;

-- Deferred to commit, so a delete-and-insert in one transaction (a permits save rewrites every Extra
-- Owner and permit role) logs nothing: only a row that no longer matches its key is gone.
CREATE OR REPLACE FUNCTION log_erasure() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
DECLARE k JSONB; w TEXT; back BOOLEAN;
BEGIN
    -- Inside a purge or a restore: skip the lookup below for each of its many rows.
    IF current_setting('tommybot.erasing', true) = 'on' THEN RETURN NULL; END IF;
    SELECT jsonb_object_agg(c, to_jsonb(OLD) -> c),
           string_agg(CASE WHEN to_jsonb(OLD) ->> c IS NULL THEN format('%I IS NULL', c)
                           ELSE format('%1$I = ($1).%1$I', c) END, ' AND ')
      INTO k, w FROM unnest(TG_ARGV) c;
    -- Plain column comparisons, so the lookup uses the table's key index.
    EXECUTE format('SELECT EXISTS (SELECT 1 FROM %I WHERE %s)', TG_TABLE_NAME, w) INTO back USING OLD;
    IF NOT back THEN PERFORM note_erasure(TG_TABLE_NAME, k); END IF;
    RETURN NULL;
END $$;

-- The key columns name the grant: a restored row that still matches them is the old grant.
-- rescue_key includes the key hash (a new key replaces it), mcp_tokens the unrevoked state.
DO $$
DECLARE r RECORD;
BEGIN
    FOR r IN SELECT * FROM (VALUES
        ('extra_owners',   'DELETE',                     ARRAY['guild_id', 'user_id']),
        ('trusted_admins', 'DELETE',                     ARRAY['guild_id', 'user_id']),
        ('permits',        'DELETE',                     ARRAY['id', 'guild_id']),
        ('permit_members', 'DELETE OR UPDATE OF permit_id', ARRAY['permit_id', 'guild_id', 'user_id']),
        ('permit_roles',   'DELETE',                     ARRAY['permit_id', 'guild_id', 'role_id']),
        ('raider_allow',   'DELETE',                     ARRAY['guild_id', 'user_id']),
        ('rescue_key',     'DELETE OR UPDATE OF key_hash', ARRAY['guild_id', 'key_hash']),
        ('dash_totp',      'DELETE',                     ARRAY['user_id']),
        ('mcp_tokens',     'DELETE OR UPDATE OF revoked_at', ARRAY['id', 'revoked_at'])
    ) v(t, ev, cols) LOOP
        EXECUTE format('DROP TRIGGER IF EXISTS erasure_log ON %I', r.t);
        EXECUTE format('CREATE CONSTRAINT TRIGGER erasure_log AFTER %s ON %I DEFERRABLE INITIALLY DEFERRED
                        FOR EACH ROW EXECUTE FUNCTION log_erasure(%s)',
                       r.ev, r.t, (SELECT string_agg(quote_literal(c), ', ') FROM unnest(r.cols) c));
    END LOOP;
END $$;

-- A person's data deletion (migration 4034) that the bot carried out: a restore queues it again.
CREATE OR REPLACE FUNCTION log_data_request() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    PERFORM note_erasure('data_request', jsonb_build_object('user_id', NEW.user_id, 'by', NEW.decided_by));
    RETURN NULL;
END $$;
-- A restore of a dump from before 4034 runs this file without the table; the bot's own run of 4035
-- after 4034 then adds the trigger.
DO $$ BEGIN
    IF to_regclass('public.data_requests') IS NOT NULL THEN
        DROP TRIGGER IF EXISTS erasure_log ON data_requests;
        CREATE TRIGGER erasure_log AFTER UPDATE OF status ON data_requests FOR EACH ROW
            -- Only the bot marks a request done: the dashboard role (internet-facing) cannot plant
            -- a deletion that a later restore would carry out. WHEN runs as the session's role.
            WHEN (NEW.kind = 'delete' AND NEW.status = 'done' AND OLD.status <> 'done'
                  AND current_user <> 'tommybot_dashboard')
            EXECUTE FUNCTION log_data_request();
    END IF;
END $$;

-- As migration 1501, plus the log row. The flag stays on to the end of the transaction, because the
-- deferred triggers of the rows it deletes run at commit (callers run it in a transaction of its own).
-- SECURITY DEFINER: the dashboard role calls it and may not write erasure_log itself.
CREATE OR REPLACE FUNCTION purge_guild(g BIGINT) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
DECLARE t TEXT;
BEGIN
    PERFORM note_erasure('purge_guild', jsonb_build_object('guild_id', g));
    PERFORM set_config('tommybot.erasing', 'on', true);
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

-- Called by scripts/restore.sh in the restore transaction, after it loaded the live log into the
-- temp table `saved`: keeps the rows newer than the dump's own, applies them, and ends every
-- dashboard login (sessions and step-ups from the dump are not the ones people hold now).
-- Returns how many log rows it applied (a row whose grant the dump never had changes nothing).
CREATE OR REPLACE FUNCTION restore_erasures() RETURNS BIGINT
LANGUAGE plpgsql SET search_path = public, pg_temp AS $$
DECLARE m BIGINT; r RECORD; n BIGINT := 0; rid BIGINT;
BEGIN
    SELECT coalesce(max(id), 0) INTO m FROM erasure_log;
    INSERT INTO erasure_log SELECT * FROM saved WHERE id > m;
    PERFORM setval(pg_get_serial_sequence('erasure_log', 'id'), max(id)) FROM erasure_log HAVING max(id) > 0;
    PERFORM set_config('tommybot.erasing', 'on', true);
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

-- SECURITY DEFINER runs as the owner (a superuser): only the bot, and the dashboard for its server
-- purge, may call these. The table is the bot's alone, also after a rollback to an image whose
-- dashboard role grants every table.
REVOKE EXECUTE ON FUNCTION note_erasure(TEXT, JSONB), purge_guild(BIGINT), restore_erasures() FROM PUBLIC;
DO $$ BEGIN
    IF EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'tommybot_dashboard') THEN
        GRANT EXECUTE ON FUNCTION purge_guild(BIGINT) TO tommybot_dashboard;
        REVOKE ALL ON erasure_log, erasure_log_id_seq FROM tommybot_dashboard;
    END IF;
END $$;
