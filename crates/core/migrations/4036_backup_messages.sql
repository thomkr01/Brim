-- Default bot messages phase 5: the Message Builder setup (saved messages, tags, schedules,
-- triggers, stickies) and the edited bot messages, stored with each backup. NULL: a backup from
-- before this, or one that only holds the server's layout; loading it leaves the messages alone.
ALTER TABLE backups ADD COLUMN messages JSONB;
