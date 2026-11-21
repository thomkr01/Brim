-- Ticket type (Report, Appeal, ...; tickets.types in the config): picked by the member when the
-- ticket opens, or by staff at claim; staff can change it. Changes go to ticket_events.
ALTER TABLE tickets ADD COLUMN kind TEXT CHECK (char_length(kind) BETWEEN 1 AND 100);
