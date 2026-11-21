-- Ticket defaults (owner decision 2026-11-21; 4054 was taken). 1) An empty or missing ticket greeting gets the default
-- greeting, so every new ticket welcomes the member; clearing it again turns it off. All
-- versions, as 4033_ticket_panel_default does.
UPDATE guild_config SET config = jsonb_set(config, '{tickets,greeting}', to_jsonb('Hi {usermention}, thanks for reaching out. Tell us what you need, with as much detail as you can, and staff will answer here soon.'::text))
WHERE jsonb_typeof(config -> 'tickets') = 'object'
  AND coalesce(config #>> '{tickets,greeting}', '') = '';
-- 2) The panel message is edited on Default messages now (entry `ticket.panel`). A panel text a
-- server wrote on the Tickets tab carries over as its edited version, in the look it had: the old
-- title, blue, the text, then the panel lines (`{text}`). An empty text or the untouched default
-- (with either bot name) gets the new default look. `tickets.panel_text` stays in old versions,
-- unread.
INSERT INTO bot_messages (guild_id, key, message, updated_by)
SELECT guild_id, 'ticket.panel',
       jsonb_build_object('content', '', 'embeds', jsonb_build_array(jsonb_build_object(
           'title', 'Open a ticket', 'color', 892408,
           'description', (config #>> '{tickets,panel_text}') || E'\n\n{text}'))),
       coalesce(changed_by, 0)
FROM (SELECT DISTINCT ON (guild_id) guild_id, config, changed_by
      FROM guild_config ORDER BY guild_id, version DESC) latest
WHERE coalesce(config #>> '{tickets,panel_text}', '') NOT IN ('', '{e:ticket} Need help from staff? Press a button below.
{e:lock} Brim opens a private channel that only you and the staff can see.
{e:note} **Have ready:** names, message links or screenshots.
{e:clock} One ticket per issue. Staff answer as soon as they can.', '{e:ticket} Need help from staff? Press a button below.
{e:lock} TommyBot opens a private channel that only you and the staff can see.
{e:note} **Have ready:** names, message links or screenshots.
{e:clock} One ticket per issue. Staff answer as soon as they can.')
ON CONFLICT (guild_id, key) DO NOTHING;
