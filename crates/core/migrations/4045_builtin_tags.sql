-- Built-in tags: a /tag run posts either a server's saved message (embed_id) or a built-in entry
-- (entry, a bot message key such as tag.dangerous_permissions), never both.
ALTER TABLE message_tag_runs ALTER COLUMN embed_id DROP NOT NULL;
ALTER TABLE message_tag_runs ADD COLUMN entry TEXT;
ALTER TABLE message_tag_runs ADD CONSTRAINT message_tag_runs_one_source
    CHECK ((embed_id IS NULL) <> (entry IS NULL));
