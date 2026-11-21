-- The default ticket panel gets three buttons and the shared embed look (owner decision 2026-11-18). Only a
-- server still on the untouched old default (one "Open a ticket" button with the old text, or no
-- text) moves to the new panels and text; a server that removed the Report or Appeal ticket type
-- keeps its panel. All versions, as 3934_panel_text does. The message in #open-a-ticket follows at
-- the server's next settings save.
UPDATE guild_config SET config = jsonb_set(jsonb_set(config,
    '{tickets,panels}', '[{"emoji":"{e:support}","enabled":true,"id":"support","name":"Get help","style":"primary"},{"emoji":"{e:report}","enabled":true,"id":"report","kind":"Report","name":"Report a member","style":"danger"},{"emoji":"{e:appeal}","enabled":true,"id":"appeal","kind":"Appeal","name":"Appeal a punishment","style":"secondary"}]'::jsonb),
    '{tickets,panel_text}', to_jsonb('{e:ticket} Need help from staff? Press a button below.
{e:lock} TommyBot opens a private channel that only you and the staff can see.
{e:note} **Have ready:** names, message links or screenshots.
{e:clock} One ticket per issue. Staff answer as soon as they can.'::text))
WHERE jsonb_array_length(CASE WHEN jsonb_typeof(config #> '{tickets,panels}') = 'array' THEN config #> '{tickets,panels}' ELSE '[]' END) = 1
  AND config #> '{tickets,panels,0}' @> '{"id": "support", "name": "Open a ticket", "style": "primary", "emoji": "{e:ticket}"}'
  AND coalesce(config #>> '{tickets,panels,0,enabled}', 'true') = 'true'
  AND coalesce(config #>> '{tickets,panels,0,description}', '') = ''
  AND coalesce(config #>> '{tickets,panels,0,greeting}', '') = ''
  AND coalesce(config #>> '{tickets,panels,0,kind}', '') = ''
  AND coalesce(config #>> '{tickets,panels,0,category}', '') = ''
  AND coalesce(config #>> '{tickets,panels,0,per_member}', '1') = '1'
  AND coalesce(jsonb_array_length(CASE WHEN jsonb_typeof(config #> '{tickets,panels,0,fields}') = 'array' THEN config #> '{tickets,panels,0,fields}' END), 0) = 0
  AND coalesce(jsonb_array_length(CASE WHEN jsonb_typeof(config #> '{tickets,panels,0,ping_roles}') = 'array' THEN config #> '{tickets,panels,0,ping_roles}' END), 0) = 0
  AND coalesce(config #>> '{tickets,panel_text}', '') IN ('', '{e:support} **Need help from staff?**
Press the button below. TommyBot opens a private channel that only you and the staff can see.
**Use it for:** {e:report} reporting a member or a message · {e:appeal} appealing a punishment · {e:question} questions · {e:message} anything else
**Have ready:** names, message links or screenshots.
Staff answer here as soon as they can. Please open one ticket per issue.')
  AND (config #> '{tickets,types}' IS NULL OR config #> '{tickets,types}' @> '["Report", "Appeal"]');
-- No panel saved yet but the old text: the bot adds the new default panels, so the text goes too
-- (the old one says "Press the button below").
UPDATE guild_config SET config = jsonb_set(config, '{tickets,panel_text}', to_jsonb('{e:ticket} Need help from staff? Press a button below.
{e:lock} TommyBot opens a private channel that only you and the staff can see.
{e:note} **Have ready:** names, message links or screenshots.
{e:clock} One ticket per issue. Staff answer as soon as they can.'::text))
WHERE coalesce(jsonb_array_length(CASE WHEN jsonb_typeof(config #> '{tickets,panels}') = 'array' THEN config #> '{tickets,panels}' END), 0) = 0
  AND config #>> '{tickets,panel_text}' = '{e:support} **Need help from staff?**
Press the button below. TommyBot opens a private channel that only you and the staff can see.
**Use it for:** {e:report} reporting a member or a message · {e:appeal} appealing a punishment · {e:question} questions · {e:message} anything else
**Have ready:** names, message links or screenshots.
Staff answer here as soon as they can. Please open one ticket per issue.';
