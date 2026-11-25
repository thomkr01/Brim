-- The golden channel's default became Softban with 7 days of messages (owner decision 2026-11-25),
-- and the owner chose that servers which saved Ban move to it too. Other saved actions stay.
UPDATE guild_config SET config = jsonb_set(config, '{catch,action}', '"Softban"')
WHERE config->'catch'->>'action' = 'Ban';
