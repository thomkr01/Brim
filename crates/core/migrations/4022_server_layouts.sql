-- Layout Guard: the saved server layout, one row per
-- version. Append-only: a change makes a new version. The dashboard keeps the newest 50 per server;
-- purge_guild deletes a removed server's rows (it finds every guild_id column).
CREATE TABLE server_layouts (
    guild_id BIGINT NOT NULL,
    version INT NOT NULL CHECK (version > 0),
    image JSONB NOT NULL,
    -- Levels and kinds on top of the image (later packages); empty for an import.
    rules JSONB NOT NULL DEFAULT '{}',
    source TEXT NOT NULL CHECK (length(source) <= 40),
    created_by BIGINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    note TEXT CHECK (length(note) <= 200),
    PRIMARY KEY (guild_id, version)
);
