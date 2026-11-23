-- Saved messages in the server's own categories, in an order the staff may set. A message
-- without a category shows under "Uncategorized", which has no row. Position 0 everywhere means
-- A to Z; a drag numbers the whole list from 1.
CREATE TABLE message_categories (
    id         BIGSERIAL PRIMARY KEY,
    guild_id   BIGINT NOT NULL,
    name       TEXT   NOT NULL,
    position   INT    NOT NULL DEFAULT 0,
    updated_by BIGINT NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (guild_id, name)
);
ALTER TABLE embeds ADD COLUMN category_id BIGINT REFERENCES message_categories(id) ON DELETE SET NULL;
ALTER TABLE embeds ADD COLUMN position INT NOT NULL DEFAULT 0;
CREATE INDEX embeds_category ON embeds (category_id);
