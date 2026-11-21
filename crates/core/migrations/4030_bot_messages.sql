-- A server's own copy of a built-in bot message (Message Builder format). No row = the default
-- shipped in the bot. `undo` keeps the copy that a revert removed, for 24 hours.
CREATE TABLE bot_messages (
    guild_id    BIGINT NOT NULL,
    key         TEXT   NOT NULL,
    message     JSONB  NOT NULL,
    updated_by  BIGINT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, key)
);
CREATE TABLE bot_messages_undo (
    guild_id    BIGINT NOT NULL,
    key         TEXT   NOT NULL,
    message     JSONB  NOT NULL,
    reverted_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, key)
);
