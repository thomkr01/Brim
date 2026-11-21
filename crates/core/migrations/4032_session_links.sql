-- security review (sessions and public links, owner decisions).
-- A login ends after 24 hours without use; the dashboard refreshes this at most every 5 minutes.
ALTER TABLE dash_session ADD COLUMN last_seen_at TIMESTAMPTZ NOT NULL DEFAULT now();

-- Uploaded pictures are served by a random link per upload, not by their hash. Pictures
-- uploaded before keep their hash as the link, so messages already posted keep their picture.
ALTER TABLE media ADD COLUMN token TEXT;
UPDATE media SET token = sha256;
ALTER TABLE media ALTER COLUMN token SET NOT NULL;
-- Not unique: a picture two servers uploaded before has the same hash link in both (the same bytes).
CREATE INDEX media_token ON media (token);

-- Rescue claims (security review): wrong codes counted on the key itself (only a holder of the key
-- can raise it), and failed claims per server for one log line per hour instead of one per try.
ALTER TABLE rescue_key ADD COLUMN code_fails INT NOT NULL DEFAULT 0, ADD COLUMN code_fails_since TIMESTAMPTZ;
CREATE TABLE rescue_claim_fails (
    guild_id BIGINT PRIMARY KEY,
    fails    INT NOT NULL,
    since    TIMESTAMPTZ NOT NULL
);
