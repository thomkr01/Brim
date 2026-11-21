-- Rename to Brim (PR B): the four erasure functions of migration 4035 again, unchanged except that the
-- flag a purge or a restore sets while it deletes is `brim.erasing` instead of `tommybot.erasing`.
-- Every function that sets the flag and every one that reads it is replaced here in one transaction,
-- and no other code (the bot, the dashboard, the scripts) sets it, so old and new never mix. A dump
-- from before this migration restores its own consistent set (scripts/restore-sql.sh runs 4035 only
-- for a dump without the log), and the next bot start runs this file on it. CREATE OR REPLACE keeps
-- each function's owner and its grants (4035: closed to PUBLIC, purge_guild open to the dashboard).

CREATE OR REPLACE FUNCTION note_erasure(t TEXT, k JSONB) RETURNS void
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
BEGIN
    -- restore_erasures and purge_guild delete many rows: their own deletes are not logged again.
    IF current_setting('brim.erasing', true) = 'on' THEN RETURN; END IF;
    DELETE FROM erasure_log WHERE at < now() - interval '35 days';
    INSERT INTO erasure_log (tbl, key) VALUES (t, k);
END $$;

CREATE OR REPLACE FUNCTION log_erasure() RETURNS trigger
LANGUAGE plpgsql SECURITY DEFINER SET search_path = public, pg_temp AS $$
DECLARE k JSONB; w TEXT; back BOOLEAN;
BEGIN
    -- Inside a purge or a restore: skip the lookup below for each of its many rows.
    IF current_setting('brim.erasing', true) = 'on' THEN RETURN NULL; END IF;
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
    PERFORM set_config('brim.erasing', 'on', true);
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
    PERFORM set_config('brim.erasing', 'on', true);
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
