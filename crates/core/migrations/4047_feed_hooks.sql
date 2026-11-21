-- Feeds: GitHub and GitLab feeds. GitHub or GitLab sends each
-- event, signed with the feed's secret, to the dashboard (`/hooks/feed/<id>`); the dashboard
-- checks it and queues it in `feed_events`; the bot posts it.
ALTER TABLE feeds DROP CONSTRAINT feeds_kind_check;
ALTER TABLE feeds ADD CONSTRAINT feeds_kind_check
    CHECK (kind IN ('rss', 'youtube', 'github', 'gitlab'));
-- The feed's random secret (GitHub's signing secret, GitLab's secret token). Never shown again
-- after it was made, never logged, never in an API list.
ALTER TABLE feeds ADD COLUMN secret TEXT;
-- The event types that post (`feedhooks::TOGGLES`).
ALTER TABLE feeds ADD COLUMN events TEXT[] NOT NULL DEFAULT '{}';
-- Events dropped over the 30 a minute limit since the bot last logged them.
ALTER TABLE feeds ADD COLUMN dropped INT NOT NULL DEFAULT 0;

-- Events to post, and for 7 days the delivery ids already taken (a repeated delivery is ignored).
CREATE TABLE feed_events (
    id       BIGSERIAL PRIMARY KEY,
    feed_id  BIGINT NOT NULL REFERENCES feeds (id) ON DELETE CASCADE,
    delivery TEXT NOT NULL,
    -- The entry `feed.github.<event>`, or 'ping' (GitHub's setup test: logged, not posted).
    event    TEXT NOT NULL,
    -- The entry's variables, made by the dashboard from the event.
    vars     JSONB NOT NULL,
    at       TIMESTAMPTZ NOT NULL DEFAULT now(),
    done     BOOLEAN NOT NULL DEFAULT false,
    -- SHA-256 of the signed body: the signature does not cover the delivery id, so the same
    -- body under a new id is a replay. Null for Brim's own "connected" row.
    body_hash TEXT,
    UNIQUE (feed_id, delivery),
    UNIQUE (feed_id, body_hash)
);
CREATE INDEX feed_events_todo ON feed_events (id) WHERE NOT done;
CREATE INDEX feed_events_at ON feed_events (feed_id, at);
