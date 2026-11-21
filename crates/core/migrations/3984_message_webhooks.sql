-- Message Builder phase 4: send as a webhook.
-- `embeds.webhook`: {"name": "...", "avatar": "https://..." | null}; null sends as Brim.
ALTER TABLE embeds ADD COLUMN webhook JSONB;
-- The webhook a copy went through, so Edit message and Delete use it.
ALTER TABLE message_sends ADD COLUMN webhook_id BIGINT;

-- One Brim webhook per channel. The token never leaves the server: not to the browser, MCP,
-- exports, backups or logs.
CREATE TABLE message_webhooks (
    channel_id BIGINT PRIMARY KEY,
    guild_id   BIGINT NOT NULL,
    webhook_id BIGINT NOT NULL,
    token      TEXT NOT NULL,
    last_used  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX message_webhooks_guild ON message_webhooks (guild_id);

-- Uploaded webhook pictures, re-encoded to PNG, served at /media/<sha256>.png for Discord to fetch.
CREATE TABLE media (
    guild_id BIGINT NOT NULL,
    sha256   TEXT NOT NULL,
    png      BYTEA NOT NULL,
    added_by BIGINT NOT NULL,
    added_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, sha256)
);
CREATE INDEX media_sha ON media (sha256);
