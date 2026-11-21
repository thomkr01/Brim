-- The limits `db::set_dashboard_role` puts on the dashboard's role, also when an older image runs.
-- An older bot image grants the role every row right again at its start (it does not know the
-- revokes), so after a rollback only these triggers keep the role from writing what only the bot
-- may set. They do nothing for any other role (the bot owns the tables; `purge_guild` and the
-- migrations run as the owner). The column grants stay: they refuse first, with no row needed.
-- TG_ARGV: the columns the dashboard may change in an UPDATE. No argument: no UPDATE at all.
-- search_path is fixed: the caller could otherwise put its own operators (pg_temp) in front.
CREATE FUNCTION dashboard_role_limit() RETURNS trigger LANGUAGE plpgsql
    SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    IF current_user <> 'tommybot_dashboard' THEN
        RETURN NEW;
    END IF;
    IF TG_TABLE_NAME = 'case_proofs' THEN
        -- Staff proof only: the #357 image names every column, so the values are checked. The id
        -- and time are the defaults (a chosen id would block the bot's next proofs).
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

CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON case_proofs
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit();
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON cases
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit('reason', 'active', 'proof');
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON reports
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit('status', 'handled_by', 'handled_at');
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON raid_log
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit();
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON joingate_actions
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit();
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE ON nuke_incidents
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_limit();

-- History and log lines the dashboard adds: never changed or deleted by it (a case's or note's
-- delete removes its lines through the foreign key, which runs as the owner; the server purge is
-- SECURITY DEFINER). A new line has the default id and time: a chosen id would take the
-- sequence's next values and block the bot's own lines, a chosen time would rewrite history.
-- TG_ARGV[0]: the time column.
CREATE FUNCTION dashboard_role_append_only() RETURNS trigger LANGUAGE plpgsql
    SET search_path = pg_catalog, pg_temp AS $$
BEGIN
    IF current_user <> 'tommybot_dashboard' THEN
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

CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE OR DELETE ON case_edits
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_append_only('edited_at');
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE OR DELETE ON note_edits
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_append_only('edited_at');
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE OR DELETE ON mod_actions
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_append_only('created_at');
CREATE TRIGGER dashboard_role_limit BEFORE INSERT OR UPDATE OR DELETE ON raider_evidence
    FOR EACH ROW EXECUTE FUNCTION dashboard_role_append_only('created_at');
