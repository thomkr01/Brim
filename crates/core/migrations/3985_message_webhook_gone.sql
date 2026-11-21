-- Message Builder: a copy whose webhook was deleted in Discord. Only that webhook could edit the
-- copy, so the Sent log and Overview say so. `hook_gone_by`: who deleted it, when the audit log says.
ALTER TABLE message_sends ADD COLUMN hook_gone_at TIMESTAMPTZ, ADD COLUMN hook_gone_by BIGINT;
CREATE INDEX message_sends_webhook ON message_sends (webhook_id) WHERE posted_id IS NOT NULL;
