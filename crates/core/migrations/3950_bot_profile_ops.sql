-- Changes to the bot's own member profile in one server (nick, bio, avatar, banner), made from the
-- dashboard by the server's Discord owner. Images are never stored: only their SHA-256.
CREATE TABLE bot_profile_ops (
    id          BIGSERIAL PRIMARY KEY,
    guild_id    BIGINT NOT NULL,
    actor_id    BIGINT NOT NULL,
    field       TEXT NOT NULL CHECK (field IN ('nick', 'bio', 'avatar', 'banner')),
    op          TEXT NOT NULL CHECK (op IN ('set', 'reset')),
    -- The nick or bio text that was set; NULL for images and resets.
    value       TEXT,
    sha256      TEXT,
    -- unknown: Discord did not answer, so the change may or may not have been made.
    status      TEXT NOT NULL DEFAULT 'reserved'
                CHECK (status IN ('reserved', 'succeeded', 'rejected', 'unknown')),
    error       TEXT,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    finished_at TIMESTAMPTZ
);
-- One change at a time per server.
CREATE UNIQUE INDEX bot_profile_ops_running ON bot_profile_ops (guild_id) WHERE status = 'reserved';
CREATE INDEX bot_profile_ops_guild ON bot_profile_ops (guild_id, created_at DESC);
