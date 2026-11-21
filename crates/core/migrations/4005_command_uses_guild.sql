-- Permit command limits count per server. Nullable so the previous image, which inserts without
-- it, still works after a rollback; rows without a guild are not seeded on restart.
ALTER TABLE command_uses ADD COLUMN guild_id BIGINT;
