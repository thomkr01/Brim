-- Rescue claim (owner decision 2026-11-20): the account that claimed a server's rescue key holds Brim's
-- Owner level there instead of the Discord owner (`discord_owner`, kept current by the bot's
-- on_owner). The Discord owner account is paused (no bot control) until the holder gives it back
-- (the row is deleted). A change of Discord owner keeps it, unless ownership lands on the holder.
-- Read by core::permits::bot_owner. The column is `server_id`, not `guild_id`, on purpose: purge_guild
-- (a data deletion, or the purge after the bot was removed) must not give the bot back to the paused
-- account. One row per server.
CREATE TABLE IF NOT EXISTS rescue_owners (
    server_id BIGINT PRIMARY KEY,
    user_id BIGINT NOT NULL,
    discord_owner BIGINT NOT NULL,
    at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (user_id <> discord_owner)
);
