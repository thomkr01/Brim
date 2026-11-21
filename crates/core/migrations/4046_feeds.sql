-- Feeds: RSS/Atom and YouTube feeds a server follows. The bot
-- polls them and posts new items through Brim's webhook in the channel (`message_webhooks`,
-- the same one the Message Builder uses), with the source's name and picture, or as the bot when
-- `send_as_bot` is on. No webhook token is stored here.
CREATE TABLE feeds (
    id             BIGSERIAL PRIMARY KEY,
    guild_id       BIGINT NOT NULL,
    channel_id     BIGINT NOT NULL,
    kind           TEXT NOT NULL CHECK (kind IN ('rss', 'youtube')),
    -- rss: the https feed address; youtube: the channel id (UC...).
    target         TEXT NOT NULL,
    ping_role      BIGINT,
    -- The owner's own webhook name and picture; null = the source's.
    name           TEXT,
    picture        TEXT,
    -- The source's name and picture, read at each check.
    source_name    TEXT NOT NULL DEFAULT '',
    source_picture TEXT,
    send_as_bot    BOOLEAN NOT NULL DEFAULT false,
    -- The feed's own edited version of its entry; null = the server's version or the default.
    message        JSONB,
    state          TEXT NOT NULL DEFAULT 'on' CHECK (state IN ('on', 'paused', 'broken')),
    fails          INT NOT NULL DEFAULT 0,
    -- False until the first check remembered the items that were already there.
    primed         BOOLEAN NOT NULL DEFAULT false,
    -- What the dashboard asked the bot to do now: 'check' or 'test' (post the newest item).
    asked          TEXT CHECK (asked IN ('check', 'test')),
    last_check     TIMESTAMPTZ,
    last_post      TIMESTAMPTZ,
    last_error     TEXT,
    created_by     BIGINT NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX feeds_guild ON feeds (guild_id);
CREATE INDEX feeds_channel ON feeds (channel_id);

-- Items a feed already saw (or posted); the newest 200 per feed are kept.
CREATE TABLE feed_seen (
    feed_id BIGINT NOT NULL REFERENCES feeds (id) ON DELETE CASCADE,
    item    TEXT NOT NULL,
    seen_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (feed_id, item)
);

-- Automod trusts a message by its exact webhook id when Brim made that webhook.
CREATE INDEX message_webhooks_id ON message_webhooks (webhook_id);
