-- Message Builder tags: staff with the "tags" permit post a saved message by its tag with /tag, in
-- the channel where they type it. The bot queues the post; the dashboard sends it like every other
-- message and writes the result back for the bot's reply.
ALTER TABLE embeds ADD COLUMN tag TEXT;
CREATE UNIQUE INDEX embeds_tag ON embeds (guild_id, tag) WHERE tag IS NOT NULL;

CREATE TABLE message_tag_runs (
    id         BIGSERIAL PRIMARY KEY,
    guild_id   BIGINT NOT NULL,
    embed_id   BIGINT NOT NULL REFERENCES embeds (id) ON DELETE CASCADE,
    channel_id BIGINT NOT NULL,
    member_id  BIGINT NOT NULL,
    -- The member's username, for the Sent log.
    member_name TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    -- Set when the dashboard claims the run (claim before action), then its answer for the member.
    claimed_at TIMESTAMPTZ,
    result     TEXT
);
-- The rate limits look back over a member's and a channel's recent runs.
CREATE INDEX message_tag_runs_member ON message_tag_runs (guild_id, member_id, created_at);
CREATE INDEX message_tag_runs_open ON message_tag_runs (id) WHERE claimed_at IS NULL;
