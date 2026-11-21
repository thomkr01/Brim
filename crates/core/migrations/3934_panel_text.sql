-- The panel message gets an editable text with a new default (owner decision 11-15). Only a server still on
-- the untouched old default panel moves to the new default panel and text. Any other server keeps
-- its message: a missing `panel_text` reads as empty. All versions, as 1600_prefix_t does.
UPDATE guild_config SET config = jsonb_set(jsonb_set(config,
    '{tickets,panels}', '[{"id": "support", "name": "Open a ticket", "enabled": true, "style": "primary", "emoji": "{e:ticket}"}]'),
    '{tickets,panel_text}', to_jsonb('{e:support} **Need help from staff?**
Press the button below. TommyBot opens a private channel that only you and the staff can see.
**Use it for:** {e:report} reporting a member or a message · {e:appeal} appealing a punishment · {e:question} questions · {e:message} anything else
**Have ready:** names, message links or screenshots.
Staff answer here as soon as they can. Please open one ticket per issue.'::text))
WHERE config #> '{tickets,panels}' = '[{"id": "support", "name": "Support", "enabled": true, "style": "primary", "description": "Press the button to open a ticket with staff."}]'::jsonb
  AND config #> '{tickets,panel_text}' IS NULL;
