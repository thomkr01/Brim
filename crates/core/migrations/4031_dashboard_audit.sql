-- Every dashboard change line, as the config-changes log channel shows it (security review
-- follow-up, owner decision 2026-11-18): kept whether or not the server has that channel, 90 days (bot
-- sweep), and removed with the server's data. `user_id` is who made the change (0: Brim itself);
-- `config_version` the config version it saved, if any.
CREATE TABLE dashboard_audit (
    id BIGSERIAL PRIMARY KEY,
    guild_id BIGINT NOT NULL,
    user_id BIGINT NOT NULL,
    text TEXT NOT NULL,
    config_version INT,
    at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX dashboard_audit_guild ON dashboard_audit (guild_id, at DESC);
CREATE INDEX dashboard_audit_at ON dashboard_audit (at);
