-- Indexes for lookups that scanned whole tables (audit 11-17): the hourly prunes and the startup
-- anti-nuke seed filter on created_at alone, the unban and appeal checks on guild + target, and
-- the dashboard bell on the signed-in user. Plain CREATE INDEX like the earlier index migrations:
-- sqlx runs a migration in a transaction, where CONCURRENTLY is refused; the build only blocks
-- writes to these tables for its duration, and it runs at start-up before the gateway connects.
CREATE INDEX IF NOT EXISTS action_log_created ON action_log (created_at);
CREATE INDEX IF NOT EXISTS action_log_target ON action_log (guild_id, target_id);
CREATE INDEX IF NOT EXISTS mod_actions_created ON mod_actions (created_at);
CREATE INDEX IF NOT EXISTS appeals_user_unseen ON appeals (user_id) WHERE seen_at IS NULL;
