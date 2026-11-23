-- Giveaway claims: each winner place has a time to press Claim. When it runs out, Brim draws that
-- place again (at most 3 times), and the new winner has 24 hours.
CREATE TABLE giveaway_claims (
    giveaway_id BIGINT NOT NULL REFERENCES giveaways (id) ON DELETE CASCADE,
    guild_id    BIGINT NOT NULL,
    place       SMALLINT NOT NULL,
    user_id     BIGINT NOT NULL,
    deadline    TIMESTAMPTZ NOT NULL,
    claimed_at  TIMESTAMPTZ,
    -- Draws of this place because nobody claimed it in time.
    redraws     SMALLINT NOT NULL DEFAULT 0,
    PRIMARY KEY (giveaway_id, place)
);
CREATE INDEX giveaway_claims_user ON giveaway_claims (giveaway_id, user_id);

-- The pause round reads the running giveaways of all servers every 15 s.
CREATE INDEX giveaways_running ON giveaways (guild_id) WHERE status = 'running';
