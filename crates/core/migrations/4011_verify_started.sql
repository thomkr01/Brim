-- Servers where verification has been turned on (owner decision 2026-11-17). The first time it is on,
-- every member already in the server gets the roles verification gives; later joiners verify.
-- `granted` is set only when every member got them; `claimed_at` marks a run in progress.
CREATE TABLE verify_started (
    guild_id BIGINT PRIMARY KEY,
    granted BOOLEAN NOT NULL DEFAULT false,
    claimed_at TIMESTAMPTZ
);

-- Verification is on already here, with roles to give: members without the role lose access and
-- get the time limit instead (owner decision 2026-11-17), so these servers count as granted.
INSERT INTO verify_started (guild_id, granted)
SELECT guild_id, true FROM (
    SELECT DISTINCT ON (guild_id) guild_id, config
    FROM guild_config
    ORDER BY guild_id, version DESC
) newest
WHERE newest.config #>> '{verification,enabled}' = 'true'
  -- CASE, because Postgres may run either side of an AND first.
  AND CASE WHEN jsonb_typeof(newest.config -> 'verification' -> 'roles') = 'array'
      THEN jsonb_array_length(newest.config -> 'verification' -> 'roles') > 0
      ELSE false END;
