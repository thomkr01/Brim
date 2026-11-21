-- security review (privacy retention and erasure).

-- NSFW scans: a clean image keeps only its hash and the threshold it was clean at, never the
-- member, channel, message or scores. Hits keep everything (staff act on them and mark them).
ALTER TABLE nsfw_scans
    ALTER COLUMN channel_id DROP NOT NULL,
    ALTER COLUMN message_id DROP NOT NULL,
    ALTER COLUMN user_id DROP NOT NULL,
    ALTER COLUMN scores DROP NOT NULL,
    ALTER COLUMN score DROP NOT NULL;
DELETE FROM nsfw_scans a WHERE NOT a.hit AND a.feedback IS NULL AND EXISTS (
    SELECT 1 FROM nsfw_scans b WHERE b.guild_id = a.guild_id AND b.sha256 = a.sha256
      AND NOT b.hit AND b.feedback IS NULL AND b.id > a.id);
UPDATE nsfw_scans SET channel_id = NULL, message_id = NULL, user_id = NULL, scores = NULL, score = NULL
    WHERE NOT hit AND feedback IS NULL;
CREATE UNIQUE INDEX nsfw_scans_clean ON nsfw_scans (guild_id, sha256) WHERE message_id IS NULL;

-- Web verification hashes now use a key the dashboard holds only in memory,
-- so the old rows (hashed with the OAuth client secret) can never match again.
DELETE FROM verify_ips;

-- A person's own request to see or delete their data, made by DM to the bot. The bot owner
-- approves or denies it on the dashboard; the bot then does it and tells the person by DM.
CREATE TABLE data_requests (
    id         BIGSERIAL PRIMARY KEY,
    user_id    BIGINT NOT NULL,
    username   TEXT NOT NULL,
    kind       TEXT NOT NULL CHECK (kind IN ('show', 'delete')),
    status     TEXT NOT NULL DEFAULT 'pending'
               CHECK (status IN ('pending', 'approved', 'denied', 'done', 'failed')),
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    decided_by BIGINT,
    decided_at TIMESTAMPTZ,
    done_at    TIMESTAMPTZ,
    -- Rows found per table when it was done (no content), for the bot owner's record.
    result     JSONB
);
CREATE UNIQUE INDEX data_requests_open ON data_requests (user_id, kind) WHERE status IN ('pending', 'approved');
CREATE INDEX data_requests_created ON data_requests (created_at);
