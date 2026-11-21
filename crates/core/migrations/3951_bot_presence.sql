-- The bot's global status and activity, set by the bot application's owner on the dashboard. One
-- row; Discord has no per-server presence. The bot reads it at every READY and on NOTIFY bot_presence.
CREATE TABLE bot_presence (
    id         BOOLEAN PRIMARY KEY DEFAULT true CHECK (id),
    status     TEXT NOT NULL CHECK (status IN ('online', 'idle', 'dnd', 'invisible')),
    kind       TEXT CHECK (kind IN ('playing', 'watching', 'listening', 'competing', 'custom')),
    text       TEXT,
    updated_by BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK ((kind IS NULL) = (text IS NULL))
);
