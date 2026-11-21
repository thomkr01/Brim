-- Owner decision 2026-11-15: a permit is given to a person by Discord user id, never by role. The roles in
-- `permit_roles` are the ones Brim gives the members; a role alone carries no permit.
-- UNIQUE (guild_id, user_id): a person holds one permit at most (owner decision).
CREATE TABLE permit_members (
    permit_id BIGINT NOT NULL REFERENCES permits(id) ON DELETE CASCADE,
    guild_id  BIGINT NOT NULL,
    user_id   BIGINT NOT NULL,
    added_by  BIGINT,
    added_at  TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (permit_id, user_id),
    UNIQUE (guild_id, user_id)
);
-- A guild is listed once the bot moved its role holders into `permit_members` (owner decision). Until
-- then every permit lookup reads the roles, as before.
CREATE TABLE permit_members_migrated (
    guild_id BIGINT PRIMARY KEY,
    at       TIMESTAMPTZ NOT NULL DEFAULT now()
);
