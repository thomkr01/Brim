-- Who queued each job and update (security review): the database
-- role of the session that wrote the row, set here and not by the writer, so no payload can claim
-- another. With its own role (DEPLOY.md) the dashboard's rows say tommybot_dashboard. A job whose
-- kind, payload or time is changed names the role that changed it; a lease or attempt does not.
-- Rows from before this, or from a dashboard still on the tommybot role, say tommybot.
ALTER TABLE scheduled_jobs ADD COLUMN producer TEXT NOT NULL DEFAULT current_user;
ALTER TABLE update_runs ADD COLUMN producer TEXT NOT NULL DEFAULT current_user;

CREATE FUNCTION job_producer() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' OR (NEW.kind, NEW.payload, NEW.run_at) IS DISTINCT FROM (OLD.kind, OLD.payload, OLD.run_at) THEN
        NEW.producer := current_user;
    ELSE
        NEW.producer := OLD.producer;
    END IF;
    RETURN NEW;
END $$;
CREATE FUNCTION run_producer() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.producer := current_user;
    ELSE
        NEW.producer := OLD.producer;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER scheduled_jobs_producer BEFORE INSERT OR UPDATE ON scheduled_jobs
    FOR EACH ROW EXECUTE FUNCTION job_producer();
CREATE TRIGGER update_runs_producer BEFORE INSERT OR UPDATE ON update_runs
    FOR EACH ROW EXECUTE FUNCTION run_producer();
