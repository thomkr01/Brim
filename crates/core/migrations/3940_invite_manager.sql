-- Invite Manager: who invited whom, one credit per member per server.
-- Every join the tracker saw, kept 90 days (pruned hourly by the bot).
CREATE TABLE invite_joins (
    id          BIGSERIAL PRIMARY KEY,
    guild_id    BIGINT NOT NULL,
    user_id     BIGINT NOT NULL,
    joined_at   TIMESTAMPTZ NOT NULL,
    code        TEXT,
    inviter_id  BIGINT,
    -- single, group, bot or unknown
    how         TEXT NOT NULL,
    -- why the invite is unknown
    reason      TEXT,
    recorded_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (guild_id, user_id, joined_at)
);
CREATE INDEX invite_joins_recorded ON invite_joins (recorded_at);

-- The first join the tracker saw of each member decides the credit; a rejoin never changes it.
CREATE TABLE invite_members (
    guild_id        BIGINT NOT NULL,
    user_id         BIGINT NOT NULL,
    first_joined_at TIMESTAMPTZ NOT NULL,
    last_joined_at  TIMESTAMPTZ NOT NULL,
    inviter_id      BIGINT,
    code            TEXT,
    how             TEXT NOT NULL,
    creditable      BOOLEAN NOT NULL DEFAULT false,
    present         BOOLEAN NOT NULL DEFAULT true,
    left_at         TIMESTAMPTZ,
    joins           INT NOT NULL DEFAULT 1,
    PRIMARY KEY (guild_id, user_id),
    CHECK (NOT creditable OR (inviter_id IS NOT NULL AND inviter_id <> user_id))
);
CREATE INDEX invite_members_inviter ON invite_members (guild_id, inviter_id) WHERE creditable;

-- Links Brim made for a member (/invites create): Discord names the bot as inviter, the
-- credit goes to the member who ran the command.
CREATE TABLE invite_links (
    guild_id   BIGINT NOT NULL,
    code       TEXT NOT NULL,
    owner_id   BIGINT NOT NULL,
    channel_id BIGINT NOT NULL,
    max_uses   INT NOT NULL,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    deleted_at TIMESTAMPTZ,
    PRIMARY KEY (guild_id, code)
);
CREATE INDEX invite_links_owner ON invite_links (guild_id, owner_id);
