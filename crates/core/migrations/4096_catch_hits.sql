-- One row per member the catch channel punished: the caught counter on the Catch channel page,
-- and later the stats page. No member id: the case of each catch already says who.
CREATE TABLE IF NOT EXISTS catch_hits (
    id       BIGSERIAL PRIMARY KEY,
    guild_id BIGINT NOT NULL,
    action   TEXT NOT NULL,
    at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS catch_hits_guild ON catch_hits (guild_id, at);
