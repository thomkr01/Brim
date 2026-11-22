-- Ticket panels (owner decision 2026-11-22). The old "Add panel" gave a new panel the placeholder
-- label "New panel" and no emoji, and members saw that button. Such a panel gets the default
-- button: "Open a ticket" with the ticket emoji, and no category of its own, so its tickets go to
-- the ticket category. The bot posts the panel message again at start. Other panel settings stay.
-- All versions, as 4055 does.
UPDATE guild_config SET config = jsonb_set(config, '{tickets,panels}', (
    SELECT jsonb_agg(CASE WHEN p ->> 'name' = 'New panel' AND coalesce(p ->> 'emoji', '') = ''
                          THEN (p - 'category') || '{"name": "Open a ticket", "emoji": "{e:ticket}"}'::jsonb
                          ELSE p END ORDER BY i)
    FROM jsonb_array_elements(config #> '{tickets,panels}') WITH ORDINALITY AS a(p, i)))
WHERE jsonb_typeof(config #> '{tickets,panels}') = 'array'
  AND EXISTS (SELECT 1 FROM jsonb_array_elements(config #> '{tickets,panels}') p
              WHERE p ->> 'name' = 'New panel' AND coalesce(p ->> 'emoji', '') = '');
