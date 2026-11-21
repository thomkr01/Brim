-- Message Builder cleanup: `embed` (the first embed, kept since 3980 so a rollback to the image
-- before the Message Builder could still read and edit a row) goes. `message` holds the whole saved message.
-- A row that image inserted or edited last gets its `message` from `embed` first.
DROP TRIGGER IF EXISTS embeds_stale_message ON embeds;
DROP FUNCTION IF EXISTS embeds_stale_message();
UPDATE embeds SET message = jsonb_build_object('content', '', 'embeds', jsonb_build_array(embed))
    WHERE message IS NULL;
ALTER TABLE embeds ALTER COLUMN message SET NOT NULL;
ALTER TABLE embeds DROP COLUMN embed;
