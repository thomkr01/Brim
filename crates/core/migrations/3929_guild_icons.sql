-- Owner decision 2026-11-15: Discord's CDN deletes a server icon as soon as it changes (live test), so an
-- icon revert or a backup restore needs Brim's own copy. One row per icon hash, stored as the
-- data URI that sets it again. Kept: the newest few per server and every icon a backup names.
CREATE TABLE guild_icons (
    guild_id BIGINT NOT NULL,
    hash     TEXT NOT NULL,
    data_uri TEXT NOT NULL,
    saved_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, hash)
);
