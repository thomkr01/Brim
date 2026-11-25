-- Ten bot messages named their one variable `{text}`. Each now has the name for what it holds
-- (`{tk_reply}`, `{cmd_usage}` ...), from `TEXT_NAMES` in `botmsg.rs`. The old name is not kept:
-- whole `{text}` tokens in the saved copy and in the copy kept for Undo.
CREATE TEMP TABLE text_rename (key TEXT PRIMARY KEY, new TEXT NOT NULL) ON COMMIT DROP;
INSERT INTO text_rename (key, new) VALUES
('ticket.sent', '{tk_message}'),
('ticket.reply', '{tk_reply}'),
('ticket.greeting', '{tk_greeting}'),
('ticket.closing', '{tk_closing}'),
('reply.ticket.usage', '{tk_usage}'),
('reply.error.usage', '{cmd_usage}'),
('reply.error.wrong_use', '{cmd_problem}'),
('reply.confirm.question', '{cmd_target}'),
('reply.lock.updates_notice', '{lock_updates}'),
('reply.lock.locked_notice', '{lock_updates}');

UPDATE bot_messages b SET message = replace(b.message::text, '{text}', r.new)::jsonb
FROM text_rename r WHERE b.key = r.key AND b.message::text LIKE '%{text}%';
UPDATE bot_messages_undo b SET message = replace(b.message::text, '{text}', r.new)::jsonb
FROM text_rename r WHERE b.key = r.key AND b.message::text LIKE '%{text}%';
