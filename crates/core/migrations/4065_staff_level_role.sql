-- The optional common Staff role: a third level role. Brim gives it to everyone at or above the
-- Moderators permit and to the Owner, Extra Owners and Trusted Admins, by user id; it gives no
-- rights. `made`: Brim made the role, so turning the feature off deletes it; a role the owner
-- picked stays, and Brim only takes it from its members.
ALTER TABLE level_roles DROP CONSTRAINT IF EXISTS level_roles_level_check;
ALTER TABLE level_roles ADD CONSTRAINT level_roles_level_check
    CHECK (level IN ('trusted_admin', 'owner', 'staff'));
ALTER TABLE level_roles ADD COLUMN made BOOLEAN NOT NULL DEFAULT false;
