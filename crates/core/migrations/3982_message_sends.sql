-- Message Builder Sent log: every copy Brim posts of a saved message (last 100 per message).
-- `body` is the saved message as that copy was last sent or edited, so its menu answers match
-- what members see on that copy. Copies posted before this start the log.
CREATE TABLE message_sends (
    id          BIGSERIAL PRIMARY KEY,
    embed_id    BIGINT NOT NULL REFERENCES embeds (id) ON DELETE CASCADE,
    guild_id    BIGINT NOT NULL,
    channel_id  BIGINT NOT NULL,
    posted_id   BIGINT,
    body        JSONB  NOT NULL,
    sent_by     BIGINT NOT NULL,
    detail      TEXT   NOT NULL,
    sent_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    edited_at   TIMESTAMPTZ
);
CREATE INDEX message_sends_embed ON message_sends (embed_id, sent_at DESC);
INSERT INTO message_sends (embed_id, guild_id, channel_id, posted_id, body, sent_by, detail, sent_at)
SELECT id, guild_id, message_channel, message_id,
       coalesce(published, message, jsonb_build_object('content', '', 'embeds', jsonb_build_array(embed))),
       updated_by, 'Sent before the Sent log', updated_at
FROM embeds WHERE message_channel IS NOT NULL AND message_id IS NOT NULL;

-- Channels made from the Message Builder's picker, for its caps (5 per hour, 25 per day per server).
CREATE TABLE message_new_channels (
    guild_id   BIGINT NOT NULL,
    channel_id BIGINT NOT NULL,
    made_by    BIGINT NOT NULL,
    made_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX message_new_channels_guild ON message_new_channels (guild_id, made_at);
