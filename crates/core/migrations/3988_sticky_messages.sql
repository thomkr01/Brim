-- Message Builder phase 7: sticky messages. A sticky is always the last message in its channel: after
-- the channel has been quiet for 3 seconds, Brim posts a new copy and deletes its old one.
CREATE TABLE sticky_msgs (
    id          BIGSERIAL PRIMARY KEY,
    embed_id    BIGINT NOT NULL REFERENCES embeds (id) ON DELETE CASCADE,
    guild_id    BIGINT NOT NULL,
    channel_id  BIGINT NOT NULL,
    -- With more than one sticky in a channel, the highest number is at the very bottom.
    position    SMALLINT NOT NULL DEFAULT 1 CHECK (position BETWEEN 1 AND 3),
    -- "Also after other bots" (Brim's own messages never count).
    bots        BOOLEAN NOT NULL DEFAULT false,
    enabled     BOOLEAN NOT NULL DEFAULT true,
    -- The copy Brim posted last and its Sent log row: the only message it may delete.
    posted_id   BIGINT,
    send_id     BIGINT,
    -- Reposts in a row caused by bot messages only; at 10 the sticky pauses.
    bot_streak  INT    NOT NULL DEFAULT 0,
    -- The last problem, shown on the sticky (channel deleted, paused, skipped repost).
    issue       TEXT,
    created_by  BIGINT NOT NULL,
    updated_by  BIGINT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT sticky_order UNIQUE (channel_id, position) DEFERRABLE INITIALLY DEFERRED,
    CONSTRAINT sticky_once UNIQUE (embed_id, channel_id) DEFERRABLE INITIALLY DEFERRED
);
CREATE INDEX sticky_msgs_guild ON sticky_msgs (guild_id);

-- A channel whose stickies are due a repost: the bot writes one row after 3 quiet seconds (with a
-- NOTIFY on message_runs); the dashboard deletes it when it claims it (claim before action).
CREATE TABLE sticky_runs (
    channel_id BIGINT PRIMARY KEY,
    guild_id   BIGINT NOT NULL,
    -- Only bot messages came in: only stickies with "Also after other bots" move down.
    bots_only  BOOLEAN NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
