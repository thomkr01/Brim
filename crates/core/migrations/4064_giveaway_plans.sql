-- Giveaway templates and plans. A template keeps the settings of a giveaway under a name. A plan
-- (spec is set) starts a giveaway from its settings on a schedule written like a message schedule
-- (`msgsched`): on a date, on repeat, or after the plan's last giveaway ends. One table, so both
-- share the columns, the role relink after a restore and the server purge.
CREATE TABLE giveaway_plans (
    id            BIGSERIAL PRIMARY KEY,
    guild_id      BIGINT NOT NULL,
    name          TEXT NOT NULL,
    -- NULL for a template: the channel is chosen when it is used.
    channel_id    BIGINT,
    prize         TEXT NOT NULL,
    host_id       BIGINT,
    sponsor_id    BIGINT,
    winners       SMALLINT NOT NULL,
    duration_secs INT NOT NULL,
    -- NULL: the server's default claim time at the time of the run.
    claim_hours   INT,
    req           JSONB NOT NULL DEFAULT '{}',
    -- Never holds an entry code: a giveaway made from it asks for a new one.
    extras        JSONB NOT NULL DEFAULT '{}',
    -- `{"tz", "start", "lines": [...]}` as in message_rules (`msgsched`); NULL for a template.
    spec          JSONB,
    -- The next run, or the next try while a run waits for the plan's giveaway to end.
    next_at       TIMESTAMPTZ,
    enabled       BOOLEAN NOT NULL DEFAULT false,
    -- A run waits until the plan's running giveaway has ended.
    wait_for_end  BOOLEAN NOT NULL DEFAULT false,
    -- A run is waiting (it starts as soon as the plan's giveaway has ended).
    held          BOOLEAN NOT NULL DEFAULT false,
    issue         TEXT,
    last_run      TIMESTAMPTZ,
    -- Who last saved it: a run uses their rights and stops when they have lost them.
    created_by    BIGINT NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    CHECK (spec IS NOT NULL OR (NOT enabled AND NOT held AND NOT wait_for_end AND next_at IS NULL AND channel_id IS NULL)),
    CHECK (spec IS NULL OR channel_id IS NOT NULL)
);
-- A template and a plan may share a name.
CREATE UNIQUE INDEX giveaway_plans_name ON giveaway_plans (guild_id, name, (spec IS NULL));
-- The runner reads the oldest due plan.
CREATE INDEX giveaway_plans_due ON giveaway_plans (next_at) WHERE enabled;

-- The plan that started a giveaway, for "start after the existing giveaway ends" (read with the
-- server id, through the index of running giveaways).
ALTER TABLE giveaways ADD COLUMN plan_id BIGINT;
