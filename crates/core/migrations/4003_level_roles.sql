-- The Discord role of the Trusted Admin list and of the Owner level (the server owner and every
-- Extra Owner, owner decision 2026-11-16). Like a permit role it gives no power: Brim gives it to the
-- people on the list by user id and takes it from everyone else.
CREATE TABLE level_roles (
    guild_id BIGINT NOT NULL,
    level    TEXT   NOT NULL CHECK (level IN ('trusted_admin', 'owner')),
    role_id  BIGINT NOT NULL,
    PRIMARY KEY (guild_id, level),
    UNIQUE (guild_id, role_id)
);
