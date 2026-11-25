-- The optional common Staff role: a third level role. Brim gives it to everyone at or above the
-- Moderators permit and to the Owner, Extra Owners and Trusted Admins, by user id; it gives no
-- rights. `made`: Brim made the role, so turning the feature off deletes it; a role the owner
-- picked stays, and Brim only takes it from its members.
ALTER TABLE level_roles DROP CONSTRAINT IF EXISTS level_roles_level_check;
ALTER TABLE level_roles ADD CONSTRAINT level_roles_level_check
    CHECK (level IN ('trusted_admin', 'owner', 'staff'));
ALTER TABLE level_roles ADD COLUMN made BOOLEAN NOT NULL DEFAULT false;
-- Roles that were the Staff role: a backup or layout taken before holds their channel access, and
-- a restore must not give it back to a role that is no longer guarded (`backup::Former`).
CREATE TABLE former_staff_roles (
    guild_id BIGINT NOT NULL,
    role_id BIGINT NOT NULL,
    -- When it stopped being the Staff role: only images taken before lose its overwrites.
    since TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (guild_id, role_id)
);
