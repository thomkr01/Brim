-- Verification's fail action defaults to Kick now (owner decision 2026-11-17: "Kick for all"). The newest
-- config of every server moves from Nothing to Kick, except Web verification, which only works
-- with Nothing; a Web config with no action saved gets Nothing written, since a missing action now
-- means Kick.
UPDATE guild_config g
SET config = jsonb_set(config, '{verification,on_fail}', '"Kick"')
WHERE config->'verification'->>'on_fail' = 'Nothing'
  AND coalesce(config->'verification'->>'mode', '') <> 'Web'
  AND version = (SELECT max(version) FROM guild_config c WHERE c.guild_id = g.guild_id);

UPDATE guild_config g
SET config = jsonb_set(config, '{verification,on_fail}', '"Nothing"')
WHERE config->'verification'->>'mode' = 'Web'
  AND NOT (config->'verification' ? 'on_fail')
  AND version = (SELECT max(version) FROM guild_config c WHERE c.guild_id = g.guild_id);
