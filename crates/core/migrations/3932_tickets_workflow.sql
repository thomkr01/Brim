-- Tickets 2.0: panel tickets beside DM tickets, claim,
-- priority and activity times. No message text is stored here (PRIVACY.md).
ALTER TABLE tickets
    ADD COLUMN source TEXT NOT NULL DEFAULT 'dm' CHECK (source IN ('dm', 'panel')),
    -- The panel's id in tickets.panels of the config; NULL for a DM ticket.
    ADD COLUMN panel TEXT,
    ADD COLUMN open_slot SMALLINT NOT NULL DEFAULT 1 CHECK (open_slot BETWEEN 1 AND 5),
    ADD COLUMN claimed_by BIGINT,
    ADD COLUMN claimed_at TIMESTAMPTZ,
    ADD COLUMN priority SMALLINT NOT NULL DEFAULT 1 CHECK (priority BETWEEN 0 AND 3),
    ADD COLUMN last_activity_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    ADD COLUMN first_staff_response_at TIMESTAMPTZ,
    ADD CONSTRAINT tickets_source_panel CHECK ((source = 'dm') = (panel IS NULL));
-- Activity was not tracked before: an open ticket counts from now, so turning on auto-close
-- does not close it at once.
UPDATE tickets SET last_activity_at = CASE WHEN closed_at IS NULL THEN now() ELSE opened_at END;

-- One open DM ticket per member and server, as before; a panel allows up to 5 open (slots).
DROP INDEX tickets_open;
CREATE UNIQUE INDEX tickets_open ON tickets (guild_id, user_id) WHERE closed_at IS NULL AND panel IS NULL;
CREATE UNIQUE INDEX tickets_open_panel ON tickets (guild_id, user_id, panel, open_slot)
    WHERE closed_at IS NULL AND panel IS NOT NULL;
CREATE INDEX tickets_active ON tickets (guild_id, last_activity_at) WHERE closed_at IS NULL;

-- Who did what to a ticket and when; never the text of a message or an answer.
CREATE TABLE ticket_events (
    id        BIGSERIAL PRIMARY KEY,
    guild_id  BIGINT NOT NULL,
    number    INT NOT NULL,
    -- NULL: the bot on its own (auto-close, a restart check).
    actor_id  BIGINT,
    action    TEXT NOT NULL,
    details   JSONB NOT NULL DEFAULT '{}',
    at        TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (guild_id, number) REFERENCES tickets (guild_id, number) ON DELETE CASCADE
);
CREATE INDEX ticket_events_by_ticket ON ticket_events (guild_id, number, at);

-- The log message that holds a closed ticket's conversation file.
CREATE TABLE ticket_transcripts (
    guild_id       BIGINT NOT NULL,
    number         INT NOT NULL,
    log_channel_id BIGINT NOT NULL,
    log_message_id BIGINT NOT NULL UNIQUE,
    -- False when the history was longer than the bot reads.
    complete       BOOLEAN NOT NULL,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, number),
    FOREIGN KEY (guild_id, number) REFERENCES tickets (guild_id, number) ON DELETE CASCADE
);

-- Published panel messages: a button opens a ticket only on a message listed here.
CREATE TABLE ticket_panel_posts (
    message_id BIGINT PRIMARY KEY,
    guild_id   BIGINT NOT NULL,
    channel_id BIGINT NOT NULL,
    panels     TEXT[] NOT NULL,
    posted_by  BIGINT NOT NULL,
    posted_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX ticket_panel_posts_guild ON ticket_panel_posts (guild_id);
