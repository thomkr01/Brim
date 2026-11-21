-- A press on a saved message's button or menu looks its copy up by the Discord message id.
CREATE INDEX message_sends_posted ON message_sends (posted_id) WHERE posted_id IS NOT NULL;
