-- Ticket panels (owner decision 2026-11-22). The default button reads "Create ticket". A panel
-- that is still exactly what 4057 made (name "Open a ticket", the ticket emoji, the primary color,
-- one ticket per member, and no description, form, greeting, category, pings or preset type) gets
-- the new name. Every other panel keeps its name, so a panel the owner changed is not touched. The
-- bot posts the panel message again at start. All versions, as 4057 does.
UPDATE guild_config SET config = jsonb_set(config, '{tickets,panels}', (
    SELECT jsonb_agg(CASE WHEN p ->> 'name' = 'Open a ticket' AND p ->> 'emoji' = '{e:ticket}'
                               AND coalesce(p ->> 'style', 'primary') = 'primary'
                               AND coalesce(p -> 'per_member', '1') = '1'::jsonb
                               AND coalesce(p ->> 'description', '') = ''
                               AND coalesce(p ->> 'greeting', '') = ''
                               AND coalesce(p ->> 'kind', '') = ''
                               AND coalesce(p -> 'fields', '[]') = '[]'
                               AND coalesce(p -> 'ping_roles', '[]') = '[]'
                               AND coalesce(p -> 'category', 'null') = 'null'
                          THEN p || '{"name": "Create ticket"}'::jsonb
                          ELSE p END ORDER BY i)
    FROM jsonb_array_elements(config #> '{tickets,panels}') WITH ORDINALITY AS a(p, i)))
WHERE jsonb_typeof(config #> '{tickets,panels}') = 'array'
  AND EXISTS (SELECT 1 FROM jsonb_array_elements(config #> '{tickets,panels}') p
              WHERE p ->> 'name' = 'Open a ticket' AND p ->> 'emoji' = '{e:ticket}');
