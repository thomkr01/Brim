-- Verification time outs and retry invites, so the "3 time outs in 24 hours = 7-day ban" count and
-- the 20 invites per server per hour cap survive a bot restart. `user_id` 0 marks a server's retry
-- invite. Rows older than a day are deleted on each write.
CREATE TABLE verify_timeouts (
    guild_id BIGINT      NOT NULL,
    user_id  BIGINT      NOT NULL,
    at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX verify_timeouts_key ON verify_timeouts (guild_id, user_id, at);
