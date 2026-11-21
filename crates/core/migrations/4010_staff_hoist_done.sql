-- Servers that got the one-time pass showing each staff role as its own group in the member
-- list (owner decision 2026-11-17). A row stops later starts from repeating it, so a role an owner
-- un-hoists afterwards stays that way.
CREATE TABLE staff_hoist_done (
    guild_id BIGINT PRIMARY KEY
);
