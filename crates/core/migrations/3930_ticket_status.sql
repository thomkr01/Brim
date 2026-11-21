-- Tickets get a state, so an open or a close that a restart cut off can be found again.
-- creating: number taken, channel not made yet. closing: the log and the channel delete are running.
ALTER TABLE tickets ADD COLUMN status TEXT NOT NULL DEFAULT 'open'
    CHECK (status IN ('creating', 'open', 'closing', 'closed'));
UPDATE tickets SET status = CASE
    WHEN closed_at IS NOT NULL THEN 'closed'
    WHEN channel_id IS NULL THEN 'creating'
    ELSE 'open'
END;
-- No CHECK tying status to closed_at yet: an image rollback still closes with closed_at only.
