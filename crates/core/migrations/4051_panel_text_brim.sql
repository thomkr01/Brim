-- The bot is called Brim (owner decision 2026-11-20). A server still on the untouched default ticket
-- panel text (written by 4033 or by the bot) gets the same text with the new name. Any edited
-- text stays. All versions, as 4033_ticket_panel_default does. The message in #open-a-ticket
-- follows at the server's next settings save.
UPDATE guild_config SET config = jsonb_set(config, '{tickets,panel_text}', to_jsonb('{e:ticket} Need help from staff? Press a button below.
{e:lock} Brim opens a private channel that only you and the staff can see.
{e:note} **Have ready:** names, message links or screenshots.
{e:clock} One ticket per issue. Staff answer as soon as they can.'::text))
WHERE config #>> '{tickets,panel_text}' = '{e:ticket} Need help from staff? Press a button below.
{e:lock} TommyBot opens a private channel that only you and the staff can see.
{e:note} **Have ready:** names, message links or screenshots.
{e:clock} One ticket per issue. Staff answer as soon as they can.';
