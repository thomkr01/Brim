-- Jobs are looked up by their member (timeout role restore on every member update, sticky,
-- appeals); without this each lookup scans the whole table.
CREATE INDEX scheduled_jobs_user ON scheduled_jobs (guild_id, kind, (payload->>'user'));
