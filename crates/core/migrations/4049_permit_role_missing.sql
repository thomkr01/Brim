-- When the bot first saw a permit's role missing from the server (a person deleted it). The
-- hourly `deadroles::sweep` drops the row once the role has been missing for `DEAD_ROLE_DAYS`: long
-- enough for an anti-nuke restore to make the role again and `relink` to point the row at it.
ALTER TABLE permit_roles ADD COLUMN missing_since TIMESTAMPTZ;
