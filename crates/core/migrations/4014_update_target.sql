-- The build the bot owner approved on the Update page (security review, owner decision 2026-11-17): the
-- commit and the registry digest of each image, so the updater pulls exactly that build and nothing
-- else. Runs queued without them (by a dashboard from before this) are refused by the updater.
ALTER TABLE update_runs ADD COLUMN target_sha TEXT, ADD COLUMN digests JSONB;

-- The commit the running updater was built from, so the dashboard can tell when the updater or
-- docker-compose.yml lags behind (they change only with the command on the server). sha_at is
-- written with it: an older updater's heartbeat keeps a stale sha but not a fresh sha_at.
ALTER TABLE updater_seen ADD COLUMN sha TEXT, ADD COLUMN sha_at TIMESTAMPTZ;
