-- Message Builder: a saved message is text plus up to 10 embeds. `embed` stays and is still written
-- (the first embed) for one release, so a rollback to the previous image keeps working.
-- Rows the previous image inserts have no `message`; readers fall back to `embed` (embeds::MSG).
ALTER TABLE embeds ADD COLUMN message JSONB;
UPDATE embeds SET message = jsonb_build_object('content', '', 'embeds', jsonb_build_array(embed));
-- After a rollback the previous image edits only `embed`; drop the then stale `message` so readers use its edit.
CREATE FUNCTION embeds_stale_message() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.embed IS DISTINCT FROM OLD.embed AND NEW.message IS NOT DISTINCT FROM OLD.message THEN
        NEW.message := NULL;
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER embeds_stale_message BEFORE UPDATE ON embeds FOR EACH ROW EXECUTE FUNCTION embeds_stale_message();
