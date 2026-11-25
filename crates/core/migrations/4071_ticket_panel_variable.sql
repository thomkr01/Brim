-- The ticket panel message names its one variable for what it holds: the panels' descriptions are
-- `{tk_panels}`, no longer `{text}` (owner rule: own variables are `function_name`). The old name
-- is not kept. Whole `{text}` tokens in the entry's saved copy and in the copy kept for Undo.
UPDATE bot_messages SET message = replace(message::text, '{text}', '{tk_panels}')::jsonb
WHERE key = 'ticket.panel' AND message::text LIKE '%{text}%';
UPDATE bot_messages_undo SET message = replace(message::text, '{text}', '{tk_panels}')::jsonb
WHERE key = 'ticket.panel' AND message::text LIKE '%{text}%';
