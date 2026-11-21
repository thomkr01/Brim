-- Access roles are gone: ticket staff need a permit with Manage Tickets (owner decision 2026-11-15). The
-- default staff permits get it, as Auto Setup now gives it to new servers.
UPDATE permits SET perms = array_append(perms, 'tickets')
WHERE name IN ('Admins', 'Moderators') AND NOT ('tickets' = ANY (perms));
