-- Message Builder: the message as Brim last posted it. A menu on Discord answers from this copy,
-- so a saved draft (not checked, not posted) never reaches members and its row and option numbers
-- always match what members see. Rows posted before this have no menus, so NULL is right for them.
ALTER TABLE embeds ADD COLUMN published JSONB;
