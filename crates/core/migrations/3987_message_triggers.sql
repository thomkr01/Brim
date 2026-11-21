-- Message Builder phase 6: trigger rules (kind 'trigger' in message_rules) and lasting message counts.

-- "Every N messages" counts per (rule, channel), written by the bot in batches so a restart keeps them.
CREATE TABLE message_counts (
    rule_id    BIGINT NOT NULL REFERENCES message_rules (id) ON DELETE CASCADE,
    channel_id BIGINT NOT NULL,
    n          BIGINT NOT NULL,
    PRIMARY KEY (rule_id, channel_id)
);

-- A trigger answer to send: the bot found a trigger in a member's message, the dashboard sends.
-- One row is one answer; the runner deletes it when it claims it (claim before action).
CREATE TABLE message_runs (
    id         BIGSERIAL PRIMARY KEY,
    rule_id    BIGINT NOT NULL REFERENCES message_rules (id) ON DELETE CASCADE,
    guild_id   BIGINT NOT NULL,
    channel_id BIGINT NOT NULL,
    member_id  BIGINT NOT NULL,
    message_id BIGINT NOT NULL,
    -- The word or pattern found, as typed in the rule (`{trigger}`, the Sent log).
    matched    TEXT   NOT NULL,
    -- 'answer': send the message; 'cooldown': tell the member by DM that the rule is on cooldown.
    kind       TEXT   NOT NULL DEFAULT 'answer' CHECK (kind IN ('answer', 'cooldown')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX message_runs_created ON message_runs (created_at);

-- A copy that deletes itself (trigger "delete the answer after"), and a copy sent by DM.
ALTER TABLE message_sends ADD COLUMN delete_at TIMESTAMPTZ, ADD COLUMN dm_user BIGINT;
CREATE INDEX message_sends_delete_at ON message_sends (delete_at) WHERE delete_at IS NOT NULL;
CREATE INDEX message_sends_dm ON message_sends (guild_id, sent_at) WHERE dm_user IS NOT NULL;

-- The hourly DM budget per server (answers and cooldown notes), kept so a restart cannot reset it.
CREATE TABLE message_dms (
    guild_id BIGINT NOT NULL,
    sent_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX message_dms_guild ON message_dms (guild_id, sent_at);
