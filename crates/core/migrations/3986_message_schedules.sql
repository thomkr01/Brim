-- Message Builder phase 5: schedule rules. Triggers (phase 6) use the same table.
CREATE TABLE message_rules (
    id          BIGSERIAL PRIMARY KEY,
    embed_id    BIGINT NOT NULL REFERENCES embeds (id) ON DELETE CASCADE,
    guild_id    BIGINT NOT NULL,
    kind        TEXT   NOT NULL CHECK (kind IN ('schedule', 'trigger')),
    enabled     BOOLEAN NOT NULL DEFAULT true,
    channel_id  BIGINT,
    -- {"tz", "start", "lines": [...]}: see crates/core/src/msgsched.rs.
    spec        JSONB  NOT NULL,
    -- The next clock run. The runner moves it on before it sends (claim before action), so one row
    -- is one chain: a save sets it again, a delete ends it.
    next_at     TIMESTAMPTZ,
    -- Set by the bot when "every N messages" is reached; the runner sends and clears it.
    due_messages BOOLEAN NOT NULL DEFAULT false,
    -- A "once" run waiting out panic mode, a lockdown or a nuke case (up to an hour).
    waiting_once BOOLEAN NOT NULL DEFAULT false,
    -- The last problem, shown on the rule (channel deleted, missing permission, skipped run).
    issue       TEXT,
    last_run    TIMESTAMPTZ,
    -- The last clock run sent: the next one waits at least 15 minutes after it.
    last_clock  TIMESTAMPTZ,
    created_by  BIGINT NOT NULL,
    updated_by  BIGINT NOT NULL,
    updated_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX message_rules_guild ON message_rules (guild_id);
CREATE INDEX message_rules_due ON message_rules (next_at) WHERE enabled AND kind = 'schedule';
CREATE INDEX message_rules_msg_due ON message_rules (id) WHERE due_messages AND enabled AND kind = 'schedule';

-- Where a copy came from: Send, or a schedule (the per-minute cap counts these).
-- `warning`: a picture Discord could not load, shown on the copy's row until it is edited or sent again.
ALTER TABLE message_sends ADD COLUMN source TEXT NOT NULL DEFAULT 'manual', ADD COLUMN rule_id BIGINT,
    ADD COLUMN warning TEXT;
CREATE INDEX message_sends_guild_source ON message_sends (guild_id, source, sent_at);
