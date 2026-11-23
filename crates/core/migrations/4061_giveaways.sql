-- Giveaways: one row per giveaway, one row per entrant. The draw runs from a `giveaway_end` job.
-- Entries of an ended or cancelled giveaway go 30 days after its end, the row itself after 90.
CREATE TABLE giveaways (
    id            BIGSERIAL PRIMARY KEY,
    guild_id      BIGINT NOT NULL,
    channel_id    BIGINT NOT NULL,
    -- NULL until the message is posted, and while it waits for a re-post.
    message_id    BIGINT,
    prize         TEXT NOT NULL,
    host_id       BIGINT,
    sponsor_id    BIGINT,
    created_by    BIGINT NOT NULL,
    winners       SMALLINT NOT NULL,
    status        TEXT NOT NULL DEFAULT 'running' CHECK (status IN ('running', 'ended', 'cancelled')),
    ends_at       TIMESTAMPTZ NOT NULL,
    ended_at      TIMESTAMPTZ,
    paused_at     TIMESTAMPTZ,
    -- Required, blocked and bypass roles, extra entries per role, minimum account age and time in the server.
    req           JSONB NOT NULL DEFAULT '{}',
    button_label  TEXT NOT NULL DEFAULT '',
    button_emoji  TEXT NOT NULL DEFAULT '',
    winner_ids    BIGINT[] NOT NULL DEFAULT '{}',
    -- Everyone who won this giveaway once: a reroll or redraw never picks them again.
    past_winners  BIGINT[] NOT NULL DEFAULT '{}',
    claim_hours   INT NOT NULL DEFAULT 48,
    needs_repost  BOOLEAN NOT NULL DEFAULT false,
    issue         TEXT,
    -- The entry count the posted message shows (-1: not posted yet), for the refresh job.
    shown_entries INT NOT NULL DEFAULT -1,
    -- Counts every draw: a confirm button made before the last draw does nothing.
    draws         INT NOT NULL DEFAULT 0,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX giveaways_guild_status ON giveaways (guild_id, status);
CREATE INDEX giveaways_message ON giveaways (message_id) WHERE message_id IS NOT NULL;

CREATE TABLE giveaway_entries (
    giveaway_id BIGINT NOT NULL REFERENCES giveaways (id) ON DELETE CASCADE,
    guild_id    BIGINT NOT NULL,
    user_id     BIGINT NOT NULL,
    weight      SMALLINT NOT NULL DEFAULT 1,
    entered_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (giveaway_id, user_id)
);
CREATE INDEX giveaway_entries_user ON giveaway_entries (guild_id, user_id);

-- The entry check asks if a member was caught in a raid or by the Join Gate during this stay, on
-- every Enter press and for every drawn winner.
CREATE INDEX raid_log_user ON raid_log (guild_id, user_id);
CREATE INDEX joingate_actions_user ON joingate_actions (guild_id, user_id);
